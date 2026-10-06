//! #1199: an authoring node gathers the known-list witnesses' cosignatures over its own newest
//! head. Real `PostgresSettlementProvider` ledger, wiremock witnesses; gated `--ignored`. Same
//! database requirements as `author_self_cosign.rs`: run with `--test-threads=1` against a
//! throwaway database whose genesis network is `NETWORK_ID`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::cosigned_sth::{verify_cosigned_tree_head, CosignedTreeHead};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::sth::SignedTreeHead;
use avalon_protocol::witness::sign_witness_cosignature;
use avalon_server::author_cosign_gather::{gather_once, AuthorGatherState};
use avalon_server::cosign_verify::WitnessCosignatureDto;
use avalon_server::nodes::{PeerInfo, WitnessAdvert};
use avalon_server::outbound_policy::OutboundPolicy;
use ed25519_dalek::{SigningKey, VerifyingKey};
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const NETWORK_ID: &str = "avalon-dev-local";
const SETTLEMENT_SEED: [u8; 32] = [9u8; 32];

struct Witness {
    key: SigningKey,
    id: String,
    server: MockServer,
    asked: Arc<AtomicUsize>,
}

#[derive(Clone, Default)]
struct Opts {
    root_override: Option<String>,
    /// Answers 404 to this many first requests.
    fail_first: usize,
    /// Cosignature `observed_at` relative to now.
    observed_offset: Option<time::Duration>,
    /// Signs with a key other than the witness's own.
    bad_signature: bool,
}

struct Cosigning {
    key: SigningKey,
    id: String,
    asked: Arc<AtomicUsize>,
    opts: Opts,
}

impl Respond for Cosigning {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let n = self.asked.fetch_add(1, Ordering::SeqCst);
        if n < self.opts.fail_first {
            return ResponseTemplate::new(404);
        }
        let size: i64 = request
            .url
            .path()
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1);
        let head = HEAD.lock().unwrap().clone().filter(|h| h.tree_size == size);
        let Some(sth) = head else {
            return ResponseTemplate::new(404);
        };
        let root = self
            .opts
            .root_override
            .clone()
            .unwrap_or(sth.root_hash.clone());
        let signer = if self.opts.bad_signature {
            SigningKey::from_bytes(&[3u8; 32])
        } else {
            self.key.clone()
        };
        let observed = OffsetDateTime::now_utc() + self.opts.observed_offset.unwrap_or_default();
        let cosig = sign_witness_cosignature(&signer, &self.id, &sth, observed).unwrap();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "layout_version": 1, "rules_version": 1, "hash_algo": 1, "extensions": "0000", "tree_size": sth.tree_size,
            "root_hash": root,
            "network_id": sth.network_id,
            "signing_key_id": sth.signing_key_id,
            "signature": sth.signature,
            "created_at": sth.created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
            "protocol_version": avalon_server::version::PROTOCOL_VERSION,
            "cosignatures": [WitnessCosignatureDto::from_witness_cosignature(&cosig)],
        }))
    }
}

static HEAD: std::sync::Mutex<Option<SignedTreeHead>> = std::sync::Mutex::new(None);

async fn witness(root_override: Option<String>) -> Witness {
    witness_with(Opts {
        root_override,
        ..Opts::default()
    })
    .await
}

async fn witness_with(opts: Opts) -> Witness {
    let key = SigningKey::generate(&mut rand::rng());
    let id = hex::encode(key.verifying_key().to_bytes());
    let asked = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/ledger/sth/\d+$"))
        .respond_with(Cosigning {
            key: key.clone(),
            id: id.clone(),
            asked: asked.clone(),
            opts,
        })
        .mount(&server)
        .await;
    Witness {
        key,
        id,
        server,
        asked,
    }
}

fn peer_for(w: &Witness) -> PeerInfo {
    let now = OffsetDateTime::now_utc();
    PeerInfo {
        identity_bound: false,
        base_url: w.server.uri(),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK_ID.to_string(),
        last_announced_at: now,
        libp2p_peer_id: None,
        libp2p_listen_addrs: Vec::new(),
        connectivity: None,
        witness: Some(WitnessAdvert {
            key_id: w.id.clone(),
            announced_at: now,
            proof: String::new(),
            direct: true,
        }),
    }
}

struct Author {
    chain: PostgresSettlementProvider,
    shard: String,
}

impl Author {
    async fn new() -> Self {
        avalon_devenv::load();
        unsafe {
            std::env::set_var(
                "AVALON_SETTLEMENT_SIGNING_KEY",
                hex::encode(SETTLEMENT_SEED),
            );
        }
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let pool = PgPoolOptions::new().connect(&url).await.unwrap();
        let chain = PostgresSettlementProvider::connect_core_shard(pool, NETWORK_ID)
            .await
            .unwrap();
        Self {
            chain,
            shard: format!("game:author-gather-{}", Uuid::new_v4().simple()),
        }
    }

    async fn commit_and_publish(&self) -> SignedTreeHead {
        let id = GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "t");
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "test.event".to_string(),
            issuer: id.clone(),
            subject: id,
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        };
        self.chain
            .commit(&EventBatch {
                id: Uuid::new_v4(),
                events: vec![event],
                created_at: OffsetDateTime::now_utc(),
            })
            .await
            .unwrap();
        let sth = self.chain.latest_signed_tree_head().await.unwrap().unwrap();
        *HEAD.lock().unwrap() = Some(sth.clone());
        sth
    }

    async fn gather(
        &self,
        policy: OutboundPolicy,
        list: &[&Witness],
        state: &mut AuthorGatherState,
    ) -> usize {
        self.gather_at(policy, list, state, OffsetDateTime::now_utc())
            .await
    }

    async fn gather_at(
        &self,
        policy: OutboundPolicy,
        list: &[&Witness],
        state: &mut AuthorGatherState,
        now: OffsetDateTime,
    ) -> usize {
        let pairs: Vec<(String, VerifyingKey)> = list
            .iter()
            .map(|w| (w.id.clone(), w.key.verifying_key()))
            .collect();
        let peers: Vec<PeerInfo> = list.iter().map(|w| peer_for(w)).collect();
        gather_once(&self.chain, policy, &pairs, &peers, &self.shard, state, now).await
    }

    async fn stored(&self, size: i64) -> Vec<avalon_protocol::witness::WitnessCosignature> {
        self.chain
            .list_witness_cosignatures(NETWORK_ID, &self.shard, size)
            .await
            .unwrap()
    }
}

fn reaches_majority(
    sth: &SignedTreeHead,
    cosignatures: Vec<avalon_protocol::witness::WitnessCosignature>,
    list: &[&Witness],
) -> bool {
    let author_key = avalon_protocol::sth::load_verify_key_from_env().unwrap();
    let known: Vec<(String, VerifyingKey)> = list
        .iter()
        .map(|w| (w.id.clone(), w.key.verifying_key()))
        .collect();
    let now = OffsetDateTime::now_utc();
    verify_cosigned_tree_head(
        &author_key,
        &CosignedTreeHead {
            sth: sth.clone(),
            cosignatures,
        },
        &known,
        now - time::Duration::minutes(10),
        now,
    )
}

#[tokio::test]
#[ignore]
async fn the_author_stores_known_list_cosignatures_so_its_head_reaches_majority() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let (w1, w2, w3) = (
        witness(None).await,
        witness(None).await,
        witness(None).await,
    );
    let list = [&w1, &w2, &w3];
    let allow = OutboundPolicy::new(true);
    let mut state = AuthorGatherState::default();

    assert!(!reaches_majority(&sth, Vec::new(), &list));
    assert_eq!(a.gather(allow, &list, &mut state).await, 3);
    let stored = a.stored(sth.tree_size).await;
    assert_eq!(stored.len(), 3);
    assert!(reaches_majority(&sth, stored, &list));

    // Fresh cosignatures are not asked for again.
    assert_eq!(a.gather(allow, &list, &mut state).await, 0);
    assert_eq!(w1.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore]
async fn a_stale_stored_cosignature_is_refreshed() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let w = witness(None).await;
    let old = sign_witness_cosignature(
        &w.key,
        &w.id,
        &sth,
        OffsetDateTime::now_utc() - time::Duration::seconds(300),
    )
    .unwrap();
    a.chain
        .store_witness_cosignature(&a.shard, &old)
        .await
        .unwrap();

    let mut state = AuthorGatherState::default();
    assert_eq!(
        a.gather(OutboundPolicy::new(true), &[&w], &mut state).await,
        1
    );
    let stored = a.stored(sth.tree_size).await;
    assert_eq!(stored.len(), 1);
    assert!(stored[0].observed_at > old.observed_at);
}

#[tokio::test]
#[ignore]
async fn a_new_head_gets_new_cosignatures() {
    let a = Author::new().await;
    let first = a.commit_and_publish().await;
    let w = witness(None).await;
    let mut state = AuthorGatherState::default();
    let allow = OutboundPolicy::new(true);
    assert_eq!(a.gather(allow, &[&w], &mut state).await, 1);
    let second = a.commit_and_publish().await;
    assert!(second.tree_size > first.tree_size);
    assert_eq!(a.gather(allow, &[&w], &mut state).await, 1);
    assert_eq!(a.stored(second.tree_size).await.len(), 1);
}

#[tokio::test]
#[ignore]
async fn a_witness_answering_for_a_different_head_is_not_stored() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let liar = witness(Some(hex::encode([7u8; 32]))).await;
    let mut state = AuthorGatherState::default();
    assert_eq!(
        a.gather(OutboundPolicy::new(true), &[&liar], &mut state)
            .await,
        0
    );
    assert!(a.stored(sth.tree_size).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn the_outbound_policy_blocks_private_witnesses() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let w = witness(None).await;
    let mut state = AuthorGatherState::default();
    assert_eq!(
        a.gather(OutboundPolicy::new(false), &[&w], &mut state)
            .await,
        0
    );
    assert_eq!(w.asked.load(Ordering::SeqCst), 0);
    assert!(a.stored(sth.tree_size).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn witnesses_known_only_from_the_directory_are_gathered_too() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let (w1, w2, w3) = (
        witness(None).await,
        witness(None).await,
        witness(None).await,
    );
    let peers: Vec<PeerInfo> = [&w1, &w2, &w3].iter().map(|w| peer_for(w)).collect();
    let mut state = AuthorGatherState::default();
    let n = gather_once(
        &a.chain,
        OutboundPolicy::new(true),
        &[],
        &peers,
        &a.shard,
        &mut state,
        OffsetDateTime::now_utc(),
    )
    .await;
    assert_eq!(n, 3);
    assert!(reaches_majority(
        &sth,
        a.stored(sth.tree_size).await,
        &[&w1, &w2, &w3]
    ));
}

/// A brand-new node: nothing stored, an empty (probationary) known list, witnesses only in the
/// peer directory. The worker's first pass runs at startup, so a majority is stored within one
/// interval of it starting (measured below).
#[tokio::test]
#[ignore]
async fn cold_start_reaches_a_served_majority_within_one_interval() {
    unsafe { std::env::set_var("AVALON_ALLOW_PRIVATE_PEERS", "true") };
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let (w1, w2, w3) = (
        witness(None).await,
        witness(None).await,
        witness(None).await,
    );
    let peers = avalon_server::nodes::PeerTable::new();
    for w in [&w1, &w2, &w3] {
        peers.upsert(peer_for(w));
    }
    let known_list = avalon_server::known_list::KnownListHandle::load_or_new(
        avalon_server::known_list::KnownListConfig::from_env(),
        None,
    );
    let started = std::time::Instant::now();
    let worker = tokio::spawn(avalon_server::author_cosign_gather::run_worker(
        a.chain.clone(),
        known_list,
        peers,
        a.shard.clone(),
    ));
    let mut elapsed = None;
    while started.elapsed() < avalon_server::author_cosign_gather::AUTHOR_GATHER_INTERVAL * 2 {
        if a.stored(sth.tree_size).await.len() == 3 {
            elapsed = Some(started.elapsed());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    worker.abort();
    let elapsed = elapsed.expect("majority never stored");
    eprintln!("cold start: majority stored after {elapsed:?}");
    assert!(elapsed < avalon_server::author_cosign_gather::AUTHOR_GATHER_INTERVAL);
    assert!(reaches_majority(
        &sth,
        a.stored(sth.tree_size).await,
        &[&w1, &w2, &w3]
    ));
}

#[tokio::test]
#[ignore]
async fn a_known_witness_holding_the_author_key_is_not_asked() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let mut same = witness(None).await;
    same.key = SigningKey::from_bytes(&SETTLEMENT_SEED);
    same.id = hex::encode(same.key.verifying_key().to_bytes());
    let other = witness(None).await;
    let mut state = AuthorGatherState::default();
    let n = a
        .gather(OutboundPolicy::new(true), &[&same, &other], &mut state)
        .await;
    assert_eq!(n, 1);
    assert_eq!(same.asked.load(Ordering::SeqCst), 0);
    assert!(reaches_majority(
        &sth,
        a.stored(sth.tree_size).await,
        &[&same, &other]
    ));
}

#[tokio::test]
#[ignore]
async fn a_witness_that_404s_on_a_young_head_is_picked_up_within_seconds() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let w = witness_with(Opts {
        fail_first: 1,
        ..Opts::default()
    })
    .await;
    let mut state = AuthorGatherState::default();
    let allow = OutboundPolicy::new(true);
    let t0 = OffsetDateTime::now_utc();
    assert_eq!(a.gather_at(allow, &[&w], &mut state, t0).await, 0);
    // Still inside the retry delay.
    assert_eq!(
        a.gather_at(allow, &[&w], &mut state, t0 + time::Duration::seconds(2))
            .await,
        0
    );
    assert_eq!(w.asked.load(Ordering::SeqCst), 1);
    assert_eq!(
        a.gather_at(allow, &[&w], &mut state, t0 + time::Duration::seconds(6))
            .await,
        1
    );
    assert_eq!(a.stored(sth.tree_size).await.len(), 1);
}

#[tokio::test]
#[ignore]
async fn out_of_range_observed_at_is_not_stored() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let old = witness_with(Opts {
        observed_offset: Some(-time::Duration::hours(1)),
        ..Opts::default()
    })
    .await;
    let future = witness_with(Opts {
        observed_offset: Some(time::Duration::minutes(10)),
        ..Opts::default()
    })
    .await;
    let mut state = AuthorGatherState::default();
    let n = a
        .gather(OutboundPolicy::new(true), &[&old, &future], &mut state)
        .await;
    assert_eq!(n, 0);
    assert!(a.stored(sth.tree_size).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn a_known_list_witness_with_a_bad_signature_is_not_stored() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let w = witness_with(Opts {
        bad_signature: true,
        ..Opts::default()
    })
    .await;
    let mut state = AuthorGatherState::default();
    assert_eq!(
        a.gather(OutboundPolicy::new(true), &[&w], &mut state).await,
        0
    );
    assert!(a.stored(sth.tree_size).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn the_per_tick_cap_limits_how_many_witnesses_are_asked() {
    use avalon_server::author_cosign_gather::MAX_ASKED_PER_TICK;
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let mut ws = Vec::new();
    for _ in 0..MAX_ASKED_PER_TICK + 4 {
        ws.push(witness(None).await);
    }
    let list: Vec<&Witness> = ws.iter().collect();
    let mut state = AuthorGatherState::default();
    let allow = OutboundPolicy::new(true);
    assert_eq!(a.gather(allow, &list, &mut state).await, MAX_ASKED_PER_TICK);
    let asked: usize = ws.iter().map(|w| w.asked.load(Ordering::SeqCst)).sum();
    assert_eq!(asked, MAX_ASKED_PER_TICK);
    assert_eq!(a.gather(allow, &list, &mut state).await, 4);
    assert_eq!(a.stored(sth.tree_size).await.len(), MAX_ASKED_PER_TICK + 4);
}

#[tokio::test]
#[ignore]
async fn a_row_the_store_leaves_untouched_is_not_counted_as_written() {
    let a = Author::new().await;
    let sth = a.commit_and_publish().await;
    let w = witness(None).await;
    // Same root, different author timestamp, older: the store accepts the call but keeps this row.
    let mut older = sth.clone();
    older.created_at -= time::Duration::seconds(1);
    let other = sign_witness_cosignature(
        &w.key,
        &w.id,
        &older,
        OffsetDateTime::now_utc() - time::Duration::seconds(300),
    )
    .unwrap();
    a.chain
        .store_witness_cosignature(&a.shard, &other)
        .await
        .unwrap();
    let mut state = AuthorGatherState::default();
    assert_eq!(
        a.gather(OutboundPolicy::new(true), &[&w], &mut state).await,
        0
    );
    let stored = a.stored(sth.tree_size).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].author_created_at, other.author_created_at);
}
