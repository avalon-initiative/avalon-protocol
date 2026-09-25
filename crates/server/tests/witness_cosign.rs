//! Exercises `avalon_server::witness_cosign::decide_and_cosign` against a
//! real Postgres instance, with a `wiremock` server standing in for the peer
//! that serves the consistency proof. Gated `--ignored`, same convention as
//! `tests/mirror_watcher.rs`.
//!
//! Every test uses its own `network_id` so nothing collides with other files'
//! fixtures on the shared database.

use avalon_chain::mirror::{self, witness_checkpoint_for};
use avalon_chain::{merkle, PostgresSettlementProvider};
use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::sth;
use avalon_protocol::witness::verify_witness_cosignature;
use avalon_server::nodes::HeadGossipTracker;
use avalon_server::witness_cosign::{decide_and_cosign, WitnessCosignConfig};
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

struct Fixture {
    pool: PgPool,
    chain: PostgresSettlementProvider,
    network_id: String,
    author: SigningKey,
    witness: SigningKey,
    config: WitnessCosignConfig,
    witness_id: String,
    /// A fixed, long-enough list of leaf hashes; heads at any size are
    /// `mth` of a prefix.
    leaves: Vec<String>,
    tracker: HeadGossipTracker,
    http: reqwest::Client,
}

impl Fixture {
    async fn new() -> Self {
        let pool = test_pool().await;
        let network_id = format!("witness-cosign-test-{}", Uuid::new_v4());
        let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());
        let witness = SigningKey::generate(&mut rand::rng());
        let witness_id = hex::encode(witness.verifying_key().to_bytes());
        let config = WitnessCosignConfig::new(witness.clone(), witness_id.clone());
        let leaves = (0u8..40).map(|i| hex::encode([i; 32])).collect();
        Self {
            pool,
            chain,
            network_id,
            author: SigningKey::generate(&mut rand::rng()),
            witness,
            config,
            witness_id,
            leaves,
            tracker: HeadGossipTracker::new(),
            http: reqwest::Client::new(),
        }
    }

    fn root_at(&self, size: usize) -> String {
        hex::encode(merkle::mth_of_hex_hashes(&self.leaves[..size]).unwrap())
    }

    fn head(&self, size: i64, root: &str) -> CosignedTreeHead {
        CosignedTreeHead {
            sth: sth::sign_tree_head(
                &self.author,
                "author-1",
                size,
                root,
                &self.network_id,
                OffsetDateTime::now_utc(),
            ),
            cosignatures: Vec::new(),
        }
    }

    async fn peer_serving_proof(&self, first: usize, second: usize) -> MockServer {
        let server = MockServer::start().await;
        let proof = merkle::consistency_proof_of_hex_hashes(first, &self.leaves[..second]).unwrap();
        Mock::given(method("GET"))
            .and(path("/ledger/proof/consistency"))
            .and(query_param("first", first.to_string()))
            .and(query_param("second", second.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "first": first,
                "second": second,
                "first_root_hash": self.root_at(first),
                "second_root_hash": self.root_at(second),
                "proof": proof.into_iter().map(hex::encode).collect::<Vec<_>>(),
            })))
            .mount(&server)
            .await;
        server
    }

    async fn run(&self, peer: &str, head: &CosignedTreeHead) {
        decide_and_cosign(
            &self.chain,
            &self.pool,
            &self.http,
            Some(&self.config),
            &self.tracker,
            peer,
            "core",
            head,
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
async fn first_ever_head_is_cosigned_unconditionally_and_checkpointed() {
    let f = Fixture::new().await;
    let head = f.head(5, &f.root_at(5));

    // No peer is contacted in the bootstrap case.
    f.run("http://127.0.0.1:1", &head).await;

    let stored = f.stored(5).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].witness_key_id, f.witness_id);
    assert!(verify_witness_cosignature(
        &f.witness.verifying_key(),
        &stored[0]
    ));
    let cp = witness_checkpoint_for(&f.pool, &f.network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((cp.tree_size, cp.root_hash), (5, f.root_at(5)));
}

#[tokio::test]
#[ignore]
async fn a_head_that_extends_the_checkpoint_by_consistency_proof_is_cosigned() {
    let f = Fixture::new().await;
    f.run("http://127.0.0.1:1", &f.head(5, &f.root_at(5))).await;

    let peer = f.peer_serving_proof(5, 12).await;
    f.run(&peer.uri(), &f.head(12, &f.root_at(12))).await;

    assert_eq!(f.stored(12).await.len(), 1);
    let cp = witness_checkpoint_for(&f.pool, &f.network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((cp.tree_size, cp.root_hash), (12, f.root_at(12)));
}

#[tokio::test]
#[ignore]
async fn a_head_that_does_not_extend_the_checkpoint_is_refused() {
    let f = Fixture::new().await;
    f.run("http://127.0.0.1:1", &f.head(5, &f.root_at(5))).await;

    // A rewritten history: a root at size 12 that is not the extension of
    // the real 5-leaf tree. The peer serves a proof for the forged pair.
    let mut forged_leaves = f.leaves.clone();
    forged_leaves[2] = hex::encode([0xEEu8; 32]);
    let forged_root = hex::encode(merkle::mth_of_hex_hashes(&forged_leaves[..12]).unwrap());
    let forged_proof = merkle::consistency_proof_of_hex_hashes(5, &forged_leaves[..12]).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ledger/proof/consistency"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "first": 5,
            "second": 12,
            "first_root_hash": f.root_at(5),
            "second_root_hash": forged_root,
            "proof": forged_proof.into_iter().map(hex::encode).collect::<Vec<_>>(),
        })))
        .mount(&server)
        .await;

    f.run(&server.uri(), &f.head(12, &forged_root)).await;

    assert!(f.stored(12).await.is_empty());
    let cp = witness_checkpoint_for(&f.pool, &f.network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cp.tree_size, 5);
}

#[tokio::test]
#[ignore]
async fn a_peer_that_cannot_serve_a_proof_gets_no_cosignature() {
    let f = Fixture::new().await;
    f.run("http://127.0.0.1:1", &f.head(5, &f.root_at(5))).await;

    f.run("http://127.0.0.1:1", &f.head(12, &f.root_at(12)))
        .await;

    assert!(f.stored(12).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn a_conflicting_root_at_an_already_cosigned_size_is_refused() {
    let f = Fixture::new().await;
    f.run("http://127.0.0.1:1", &f.head(5, &f.root_at(5))).await;

    let conflicting = f.head(5, &hex::encode([0x77u8; 32]));
    f.run("http://127.0.0.1:1", &conflicting).await;

    let stored = f.stored(5).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].root_hash, f.root_at(5));
    let cp = witness_checkpoint_for(&f.pool, &f.network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cp.root_hash, f.root_at(5));
}

#[tokio::test]
#[ignore]
async fn re_observing_the_same_head_does_not_produce_a_second_cosignature() {
    let f = Fixture::new().await;
    let head = f.head(5, &f.root_at(5));
    f.run("http://127.0.0.1:1", &head).await;
    let first_sig = f.stored(5).await[0].signature.clone();

    f.run("http://127.0.0.1:1", &head).await;

    let stored = f.stored(5).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].signature, first_sig);
}

#[tokio::test]
#[ignore]
async fn a_shard_marked_equivocating_gets_no_cosignature() {
    let f = Fixture::new().await;
    f.tracker.mark_equivocating("core");

    f.run("http://127.0.0.1:1", &f.head(5, &f.root_at(5))).await;

    assert!(f.stored(5).await.is_empty());
    assert!(witness_checkpoint_for(&f.pool, &f.network_id, "core")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore]
async fn no_config_means_no_cosignature() {
    let f = Fixture::new().await;
    decide_and_cosign(
        &f.chain,
        &f.pool,
        &f.http,
        None,
        &f.tracker,
        "http://127.0.0.1:1",
        "core",
        &f.head(5, &f.root_at(5)),
    )
    .await;

    assert!(f.stored(5).await.is_empty());
    let _ = mirror::witness_checkpoint_for(&f.pool, &f.network_id, "core").await;
}
