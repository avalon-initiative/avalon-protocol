//! #1165: entries written by a real `PostgresSettlementProvider` mirror through the watcher.

use super::*;
use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::events::{EventBatch, IdentityChainPosition};
use sqlx::postgres::PgPoolOptions;
use wiremock::{matchers, Mock, MockServer, Request, ResponseTemplate};

const TABLES: [&str; 7] = [
    "ledger_entries",
    "ledger_batches",
    "signed_tree_heads",
    "chain_genesis",
    "mirrored_entries",
    "observed_sths",
    "equivocation_findings",
];

/// A schema holding clones of the migrated `public` ledger and mirror tables.
struct Schema {
    name: String,
    pool: PgPool,
    admin: PgPool,
}

impl Schema {
    async fn new(label: &str) -> Self {
        avalon_devenv::load();
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let admin = PgPoolOptions::new().connect(&url).await.unwrap();
        let name = format!("rw_{label}_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {name}")))
            .execute(&admin)
            .await
            .unwrap();
        for table in TABLES {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE {name}.{table} (LIKE public.{table} INCLUDING ALL)"
            )))
            .execute(&admin)
            .await
            .unwrap();
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {name}.ledger_entries ADD CONSTRAINT ledger_entries_batch_id_fkey \
             FOREIGN KEY (batch_id) REFERENCES {name}.ledger_batches(batch_id) \
             DEFERRABLE INITIALLY DEFERRED"
        )))
        .execute(&admin)
        .await
        .unwrap();
        let owned = name.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |conn, _| {
                let name = owned.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "SET search_path = {name}, public"
                    )))
                    .execute(conn)
                    .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        Self { name, pool, admin }
    }

    async fn drop(self) {
        self.pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

fn event(i: usize, payload: serde_json::Value, position: bool) -> ProtocolEvent {
    let actor = Uuid::new_v4().to_string();
    let id = GlobalId::new("identity", &actor, "self", "test_event");
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: format!("test.event_{i}"),
        issuer: id.clone(),
        subject: id,
        payload,
        // Sub-second precision on purpose: only whole seconds are hashed.
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
        identity_chain: position.then_some(IdentityChainPosition {
            seq: 1,
            prev_hash: None,
        }),
    }
}

#[tokio::test]
#[ignore]
async fn entries_written_by_a_real_authority_mirror_completely() {
    let authority = Schema::new("auth").await;
    let mirror_db = Schema::new("mirror").await;
    let network_id = format!("avalon-test-real-{}", Uuid::new_v4());
    let chain = PostgresSettlementProvider::connect(authority.pool.clone(), &network_id)
        .await
        .expect("genesis");

    let payloads = [
        serde_json::json!({ "plain": "x" }),
        serde_json::json!({ "f": 1.5, "neg": -0.25, "e": 1e-7, "big": 12345678901234u64,
            "nested": { "a": [1, 2.75, { "b": null }], "s": "caf\u{e9} \u{1F600}" } }),
        serde_json::json!({ "zeros": [0.0, 100.0, 2.50], "unordered": { "z": 1, "a": 2 } }),
        serde_json::json!(null),
        serde_json::json!([1, 2, 3]),
        serde_json::json!("just a string"),
    ];
    for chunk in [&payloads[..2], &payloads[2..3], &payloads[3..]] {
        let events: Vec<ProtocolEvent> = chunk
            .iter()
            .enumerate()
            .map(|(i, p)| event(i, p.clone(), i % 2 == 0))
            .collect();
        chain
            .commit(&EventBatch {
                id: Uuid::new_v4(),
                events,
                created_at: OffsetDateTime::now_utc(),
            })
            .await
            .expect("commit");
    }

    let views = chain.list_entries().await.unwrap();
    let sth = chain.latest_signed_tree_head().await.unwrap().unwrap();
    assert_eq!(sth.tree_size as usize, views.len());
    let hashes: Vec<String> = views.iter().map(|v| v.entry_hash.clone()).collect();
    let served: Vec<serde_json::Value> = views
        .into_iter()
        .map(|v| serde_json::to_value(crate::settlement::LedgerEntryResponse::from(v)).unwrap())
        .collect();

    let server = MockServer::start().await;
    let entries = served.clone();
    Mock::given(matchers::path("/ledger/entries"))
        .respond_with(move |req: &Request| {
            let since: i64 = req
                .url
                .query_pairs()
                .find(|(k, _)| k == "since_seq")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let page: Vec<_> = entries
                .iter()
                .filter(|e| e["seq"].as_i64().unwrap() > since)
                .collect();
            ResponseTemplate::new(200).set_body_json(page)
        })
        .mount(&server)
        .await;
    let (labels, proof_hashes, root) = (
        served
            .iter()
            .map(|e| e["seq"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        hashes.clone(),
        sth.root_hash.clone(),
    );
    Mock::given(matchers::path("/ledger/proof/inclusion"))
        .respond_with(move |req: &Request| {
            let seq: i64 = req
                .url
                .query_pairs()
                .find(|(k, _)| k == "seq")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap();
            let idx = labels.iter().position(|l| *l == seq).unwrap();
            let proof = merkle::inclusion_proof_of_hex_hashes(idx, &proof_hashes).unwrap();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "root_hash": root, "leaf_hash": proof_hashes[idx],
                "proof": proof.iter().map(hex::encode).collect::<Vec<_>>(),
            }))
        })
        .mount(&server)
        .await;

    let result = backfill(
        &crate::node_http::NodeClient::new(),
        &mirror_db.pool,
        &PostgresIndexer::new(mirror_db.pool.clone()),
        mirror::CORE_SHARD_ID,
        &[server.uri()],
        &sth,
        None,
        false,
    )
    .await;
    let stored: Vec<(i64, String)> =
        sqlx::query_as("SELECT seq, entry_hash FROM mirrored_entries ORDER BY seq")
            .fetch_all(&mirror_db.pool)
            .await
            .unwrap();
    authority.drop().await;
    mirror_db.drop().await;
    result.expect("backfill of real authority entries");
    assert_eq!(
        stored.iter().map(|(_, h)| h.clone()).collect::<Vec<_>>(),
        hashes
    );
}
