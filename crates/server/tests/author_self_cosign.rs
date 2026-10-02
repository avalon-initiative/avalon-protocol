//! #1122: the node that authors a shard cosigns its own heads with its witness key. Runs a real
//! `PostgresSettlementProvider` ledger; gated `--ignored`, and the `--test-threads=1` run should
//! point `DATABASE_URL` at a throwaway database since the ledger tables are not network-scoped.

use avalon_chain::mirror::{record_witness_checkpoint, witness_checkpoint_for};
use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::cosigned_sth::{verify_cosigned_tree_head, CosignedTreeHead};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::witness::verify_witness_cosignature;
use avalon_server::nodes::HeadGossipTracker;
use avalon_server::witness_cosign::{cosign_own_latest_head, WitnessCosignConfig};
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const SETTLEMENT_SEED: [u8; 32] = [9u8; 32];

struct Author {
    pool: PgPool,
    chain: PostgresSettlementProvider,
    network_id: String,
    config: WitnessCosignConfig,
    tracker: HeadGossipTracker,
}

impl Author {
    async fn new() -> Self {
        avalon_devenv::load();
        // Same value in every test of this binary, so concurrent set_var is harmless.
        unsafe {
            std::env::set_var(
                "AVALON_SETTLEMENT_SIGNING_KEY",
                hex::encode(SETTLEMENT_SEED),
            );
        }
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let pool = PgPoolOptions::new().connect(&url).await.unwrap();
        let network_id = format!("self-cosign-{}", Uuid::new_v4());
        let chain = PostgresSettlementProvider::connect(pool.clone(), &network_id)
            .await
            .unwrap();
        let witness = SigningKey::generate(&mut rand::rng());
        let id = hex::encode(witness.verifying_key().to_bytes());
        Self {
            pool,
            chain,
            network_id,
            config: WitnessCosignConfig::new(witness, id),
            tracker: HeadGossipTracker::new(),
        }
    }

    async fn commit(&self, events: usize) {
        let events = (0..events)
            .map(|i| {
                let id = GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "t");
                ProtocolEvent {
                    id: Uuid::new_v4(),
                    kind: format!("test.event_{i}"),
                    issuer: id.clone(),
                    subject: id,
                    payload: serde_json::json!({ "i": i }),
                    timestamp: OffsetDateTime::now_utc(),
                    version: 1,
                    identity_chain: None,
                }
            })
            .collect();
        self.chain
            .commit(&EventBatch {
                id: Uuid::new_v4(),
                events,
                created_at: OffsetDateTime::now_utc(),
            })
            .await
            .unwrap();
    }

    async fn cosign(&self) {
        cosign_own_latest_head(
            &self.chain,
            &self.pool,
            Some(&self.config),
            &self.tracker,
            "core",
        )
        .await;
    }

    async fn stored(&self, size: i64) -> Vec<avalon_protocol::witness::WitnessCosignature> {
        self.chain
            .list_witness_cosignatures(&self.network_id, "core", size)
            .await
            .unwrap()
    }
}

#[tokio::test]
#[ignore]
async fn the_authoring_node_cosigns_its_new_head_and_it_counts_toward_majority() {
    let a = Author::new().await;
    a.commit(3).await;
    a.cosign().await;

    let sth = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    let stored = a.stored(sth.tree_size).await;
    assert_eq!(stored.len(), 1);
    let witness_key = a.config.key_id();
    assert_eq!(stored[0].witness_key_id, witness_key);
    let verifying = {
        let bytes = hex::decode(witness_key).unwrap();
        ed25519_dalek::VerifyingKey::from_bytes(&bytes.try_into().unwrap()).unwrap()
    };
    assert!(verify_witness_cosignature(&verifying, &stored[0]));

    // A 2-witness list {other, author's witness key} reaches majority only with this cosignature.
    let author_key = avalon_protocol::sth::load_verify_key_from_env().unwrap();
    let other = SigningKey::generate(&mut rand::rng());
    let other_id = hex::encode(other.verifying_key().to_bytes());
    let known = vec![
        (other_id.clone(), other.verifying_key()),
        (witness_key.to_string(), verifying),
    ];
    let other_cosig = avalon_protocol::witness::sign_witness_cosignature(
        &other,
        &other_id,
        sth.tree_size,
        &sth.root_hash,
        &sth.network_id,
        sth.created_at,
        OffsetDateTime::now_utc(),
    );
    let accepts = |cosignatures| {
        let head = CosignedTreeHead {
            sth: sth.clone(),
            cosignatures,
        };
        let now = OffsetDateTime::now_utc();
        verify_cosigned_tree_head(
            &author_key,
            &head,
            &known,
            now - time::Duration::hours(1),
            now,
        )
    };
    assert!(!accepts(vec![other_cosig.clone()]));
    assert!(accepts(vec![other_cosig, stored[0].clone()]));
}

#[tokio::test]
#[ignore]
async fn a_restart_cosigns_the_existing_head_and_later_heads_extend_it() {
    let a = Author::new().await;
    a.commit(2).await;
    a.commit(2).await;
    // Heads committed before the node ever cosigned: the first tick signs the latest one.
    a.cosign().await;
    let first = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    assert_eq!(a.stored(first.tree_size).await.len(), 1);

    // Repeating (a restart's first tick) adds nothing.
    a.cosign().await;
    assert_eq!(a.stored(first.tree_size).await.len(), 1);

    a.commit(3).await;
    a.cosign().await;
    let next = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    assert!(next.tree_size > first.tree_size);
    assert_eq!(a.stored(next.tree_size).await.len(), 1);
    let cp = witness_checkpoint_for(&a.pool, &a.network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (cp.tree_size, cp.root_hash),
        (next.tree_size, next.root_hash)
    );
}

#[tokio::test]
#[ignore]
async fn a_smaller_or_forked_checkpoint_blocks_cosigning_the_head() {
    let a = Author::new().await;
    a.commit(4).await;
    let sth = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    let id = a.config.key_id();
    let record = |size: i64, root: String| {
        let (pool, net, id) = (a.pool.clone(), a.network_id.clone(), id.to_string());
        async move {
            record_witness_checkpoint(&pool, &net, "core", size, &root, &id)
                .await
                .unwrap()
        }
    };

    // Checkpoint ahead of the head: never cosign a smaller size.
    record(sth.tree_size + 10, "aa".repeat(32)).await;
    a.cosign().await;
    assert!(a.stored(sth.tree_size).await.is_empty());

    // Same size, different root: the double-cosign guard holds.
    record(sth.tree_size, "bb".repeat(32)).await;
    a.cosign().await;
    assert!(a.stored(sth.tree_size).await.is_empty());

    // An earlier checkpoint whose root is not a prefix of this ledger: consistency fails.
    record(1, "cc".repeat(32)).await;
    a.cosign().await;
    assert!(a.stored(sth.tree_size).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn no_config_or_an_equivocating_shard_means_no_self_cosignature() {
    let a = Author::new().await;
    a.commit(2).await;
    cosign_own_latest_head(&a.chain, &a.pool, None, &a.tracker, "core").await;
    a.tracker.mark_equivocating("core");
    a.cosign().await;
    let sth = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    assert!(a.stored(sth.tree_size).await.is_empty());
}
