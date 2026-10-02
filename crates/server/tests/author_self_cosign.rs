//! #1122: the node that authors a shard cosigns its own heads with its witness key. Runs a real
//! `PostgresSettlementProvider` ledger; gated `--ignored`. The ledger and its genesis are one per
//! database, so run with `--test-threads=1` against a throwaway database whose genesis network is
//! `NETWORK_ID` (a fresh `avalon-server-bundled` data dir); each test uses its own shard id.

use avalon_chain::mirror::{record_witness_checkpoint, witness_checkpoint_for};
use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::cosigned_sth::{verify_cosigned_tree_head, CosignedTreeHead};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::witness::verify_witness_cosignature;
use avalon_server::nodes::HeadGossipTracker;
use avalon_server::witness_cosign::{cosign_own_latest_head, CosignOutcome, WitnessCosignConfig};
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// The genesis network of a freshly bootstrapped dev database.
const NETWORK_ID: &str = "avalon-dev-local";
const SETTLEMENT_SEED: [u8; 32] = [9u8; 32];

struct Author {
    pool: PgPool,
    chain: PostgresSettlementProvider,
    network_id: String,
    shard: String,
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
        let network_id = NETWORK_ID.to_string();
        let shard = format!("game:self-cosign-{}", Uuid::new_v4().simple());
        let chain = PostgresSettlementProvider::connect(pool.clone(), &network_id)
            .await
            .unwrap();
        let witness = SigningKey::generate(&mut rand::rng());
        let id = hex::encode(witness.verifying_key().to_bytes());
        Self {
            pool,
            chain,
            network_id,
            shard,
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

    async fn cosign(&self) -> CosignOutcome {
        self.cosign_shard(&self.shard).await
    }

    async fn cosign_shard(&self, shard: &str) -> CosignOutcome {
        cosign_own_latest_head(
            &self.chain,
            &self.pool,
            Some(&self.config),
            &self.tracker,
            shard,
        )
        .await
    }

    async fn stored(&self, size: i64) -> Vec<avalon_protocol::witness::WitnessCosignature> {
        self.stored_for(&self.shard, size).await
    }

    async fn stored_for(
        &self,
        shard: &str,
        size: i64,
    ) -> Vec<avalon_protocol::witness::WitnessCosignature> {
        self.chain
            .list_witness_cosignatures(&self.network_id, shard, size)
            .await
            .unwrap()
    }

    fn fresh_shard() -> String {
        format!("game:self-cosign-{}", Uuid::new_v4().simple())
    }

    async fn checkpoint(&self, shard: &str) -> Option<(i64, String)> {
        witness_checkpoint_for(&self.pool, &self.network_id, shard)
            .await
            .unwrap()
            .map(|c| (c.tree_size, c.root_hash))
    }

    async fn set_checkpoint(&self, shard: &str, size: i64, root: &str) {
        record_witness_checkpoint(
            &self.pool,
            &self.network_id,
            shard,
            size,
            root,
            self.config.key_id(),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
#[ignore]
async fn the_authoring_node_cosigns_its_new_head_and_it_counts_toward_majority() {
    let a = Author::new().await;
    a.commit(3).await;
    assert_eq!(a.cosign().await, CosignOutcome::Cosigned);

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
    let cp = witness_checkpoint_for(&a.pool, &a.network_id, &a.shard)
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
async fn a_checkpoint_ahead_of_the_head_is_not_forward() {
    let a = Author::new().await;
    a.commit(4).await;
    let sth = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    // Control: a shard with no checkpoint cosigns this very head.
    assert_eq!(
        a.cosign_shard(&Author::fresh_shard()).await,
        CosignOutcome::Cosigned
    );

    let ahead = (sth.tree_size + 10, "aa".repeat(32));
    a.set_checkpoint(&a.shard, ahead.0, &ahead.1).await;
    assert_eq!(a.cosign().await, CosignOutcome::NotForward);
    assert!(a.stored(sth.tree_size).await.is_empty());
    assert_eq!(a.checkpoint(&a.shard).await, Some(ahead));
}

#[tokio::test]
#[ignore]
async fn a_different_root_at_the_checkpointed_size_is_a_double_cosign_conflict() {
    let a = Author::new().await;
    a.commit(4).await;
    let sth = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    assert_eq!(
        a.cosign_shard(&Author::fresh_shard()).await,
        CosignOutcome::Cosigned
    );

    let other = "bb".repeat(32);
    a.set_checkpoint(&a.shard, sth.tree_size, &other).await;
    assert_eq!(a.cosign().await, CosignOutcome::ConflictingRoot);
    assert!(a.stored(sth.tree_size).await.is_empty());
    assert_eq!(a.checkpoint(&a.shard).await, Some((sth.tree_size, other)));
}

#[tokio::test]
#[ignore]
async fn a_checkpoint_that_is_not_a_prefix_of_the_ledger_fails_the_consistency_check() {
    let a = Author::new().await;
    a.commit(2).await;
    let early = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    a.commit(3).await;
    let head = a.chain.latest_signed_tree_head().await.unwrap().unwrap();
    assert!(head.tree_size > early.tree_size);

    // Control: a checkpoint holding this ledger's true earlier root extends cleanly.
    let honest = Author::fresh_shard();
    a.set_checkpoint(&honest, early.tree_size, &early.root_hash)
        .await;
    assert_eq!(a.cosign_shard(&honest).await, CosignOutcome::Cosigned);
    assert_eq!(a.stored_for(&honest, head.tree_size).await.len(), 1);

    // Same size, wrong root: the own-ledger proof cannot verify against it.
    let bogus = "cc".repeat(32);
    a.set_checkpoint(&a.shard, early.tree_size, &bogus).await;
    assert_eq!(a.cosign().await, CosignOutcome::ConsistencyFailed);
    assert!(a.stored(head.tree_size).await.is_empty());
    assert_eq!(a.checkpoint(&a.shard).await, Some((early.tree_size, bogus)));
}

#[tokio::test]
#[ignore]
async fn no_config_or_an_equivocating_shard_means_no_self_cosignature() {
    let a = Author::new().await;
    a.commit(2).await;
    let sth = a.chain.latest_signed_tree_head().await.unwrap().unwrap();

    let none = cosign_own_latest_head(&a.chain, &a.pool, None, &a.tracker, &a.shard).await;
    assert_eq!(none, CosignOutcome::Disabled);

    // Control: an unmarked shard cosigns this head; the marked one does not.
    assert_eq!(
        a.cosign_shard(&Author::fresh_shard()).await,
        CosignOutcome::Cosigned
    );
    a.tracker.mark_equivocating(&a.shard);
    assert_eq!(a.cosign().await, CosignOutcome::Equivocating);
    assert!(a.stored(sth.tree_size).await.is_empty());
    assert_eq!(a.checkpoint(&a.shard).await, None);
}
