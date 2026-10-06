//! A mirror whose known list has confirmed witnesses must keep following an
//! author that serves no cosignatures, gathering them from the witnesses
//! directly. Real Postgres and the real mirror worker; the author and the
//! three cosigning witnesses are wiremock servers. Gated `--ignored`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_protocol::network_trust::{NetworkEnvironment, TrustAnchorEntry};
use avalon_protocol::sth::{self, SignedTreeHead};
use avalon_protocol::witness::sign_witness_cosignature;
use avalon_server::cosign_verify::WitnessCosignatureDto;
use avalon_server::known_list::{DiscoveredCandidate, KnownListConfig, KnownListHandle};
use avalon_server::mirror_watcher::{run_worker, MirrorWatcherConfig, MirrorWatcherHandles};
use avalon_server::nodes::{HeadGossipTracker, PeerInfo, PeerTable, ShardRegistry, WitnessAdvert};
use avalon_server::witness_cosign::WitnessCosignConfig;
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

type Heads = Arc<Mutex<HashMap<i64, SignedTreeHead>>>;

fn sth_json(sth: &SignedTreeHead, cosignatures: Vec<WitnessCosignatureDto>) -> serde_json::Value {
    serde_json::json!({
        "tree_size": sth.tree_size,
        "root_hash": sth.root_hash,
        "network_id": sth.network_id,
        "signing_key_id": sth.signing_key_id,
        "signature": sth.signature,
        "created_at": sth.created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "protocol_version": avalon_server::version::PROTOCOL_VERSION,
        "cosignatures": cosignatures,
    })
}

struct AuthorLatest(Heads);

impl Respond for AuthorLatest {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let heads = self.0.lock().unwrap();
        match heads.keys().max().and_then(|k| heads.get(k)) {
            Some(sth) => ResponseTemplate::new(200).set_body_json(sth_json(sth, Vec::new())),
            None => ResponseTemplate::new(404),
        }
    }
}

struct WitnessAtSize {
    heads: Heads,
    key: SigningKey,
    id: String,
}

impl Respond for WitnessAtSize {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let size: i64 = request
            .url
            .path()
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1);
        let heads = self.heads.lock().unwrap();
        let Some(sth) = heads.get(&size) else {
            return ResponseTemplate::new(404);
        };
        let cosig = sign_witness_cosignature(
            &self.key,
            &self.id,
            sth.tree_size,
            &sth.root_hash,
            &sth.network_id,
            sth.created_at,
            OffsetDateTime::now_utc(),
        );
        ResponseTemplate::new(200).set_body_json(sth_json(
            sth,
            vec![WitnessCosignatureDto::from_witness_cosignature(&cosig)],
        ))
    }
}

#[tokio::test]
#[ignore = "needs live Postgres"]
async fn a_mirror_with_confirmed_witnesses_keeps_following_an_author_that_serves_no_cosignatures() {
    avalon_devenv::load();
    std::env::set_var("AVALON_ALLOW_PRIVATE_PEERS", "true");
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Postgres must be reachable");

    let network_id = format!("cosign-gather-{}", Uuid::new_v4());
    let author = SigningKey::generate(&mut rand::rng());
    let heads: Heads = Arc::new(Mutex::new(HashMap::new()));

    let author_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ledger/sth/latest"))
        .respond_with(AuthorLatest(heads.clone()))
        .mount(&author_server)
        .await;

    let now = OffsetDateTime::now_utc();
    let peers = PeerTable::new();
    let mut candidates = Vec::new();
    let mut servers = Vec::new();
    for i in 0..3u8 {
        let key = SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/ledger/sth/\d+$"))
            .respond_with(WitnessAtSize {
                heads: heads.clone(),
                key,
                id: id.clone(),
            })
            .mount(&server)
            .await;
        peers.upsert(PeerInfo {
            identity_bound: false,
            base_url: server.uri(),
            roles: vec!["combined".to_string()],
            protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
            network_id: network_id.clone(),
            last_announced_at: now,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
            connectivity: None,
            witness: Some(WitnessAdvert {
                key_id: id.clone(),
                announced_at: now,
                proof: String::new(),
                direct: true,
            }),
        });
        candidates.push(DiscoveredCandidate {
            witness_key_id: id,
            prefix: format!("prefix-{i}"),
            is_anchor: false,
        });
        servers.push(server);
    }

    let known_list = KnownListHandle::load_or_new(
        KnownListConfig {
            capacity: 10,
            anchor_capacity: 0,
            max_per_prefix: 10,
            freshness_window: Duration::from_secs(3600),
            probation_window: Duration::from_secs(0),
            freshness_floor: 1.0,
        },
        None,
    );
    known_list.tick(&candidates, now);
    known_list.tick(&candidates, now);
    assert_eq!(known_list.confirmed_witness_key_ids().len(), 3);

    let own_witness_key = SigningKey::generate(&mut rand::rng());
    let own_witness_id = hex::encode(own_witness_key.verifying_key().to_bytes());
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network_id.clone());
    let config = MirrorWatcherConfig {
        peers: vec![("core".to_string(), author_server.uri())],
        poll_interval: Duration::from_millis(300),
        known_shard_ids: BTreeSet::from(["core".to_string()]),
        auto_mirror_discovered: false,
    };
    let handles = MirrorWatcherHandles {
        interest: avalon_server::interest::InterestRegistry::new().0,
        shard_registry: ShardRegistry::new(),
        own_base_url: None,
        wake: Arc::new(tokio::sync::Notify::new()),
        own_shard_id: "mirror-own-shard".to_string(),
        known_list,
        head_gossip: HeadGossipTracker::new(),
        witness: Some(WitnessCosignConfig::new(
            own_witness_key,
            own_witness_id.clone(),
        )),
        peers,
        trust_anchors: vec![TrustAnchorEntry {
            label: network_id.clone(),
            network_id: network_id.clone(),
            verify_key: hex::encode(author.verifying_key().to_bytes()),
            signing_key_id: "author-1".to_string(),
            server_url: None,
            environment: NetworkEnvironment::LocalDev,
            seed_nodes: Vec::new(),
            notes: None,
        }],
    };
    let worker = tokio::spawn(run_worker(
        pool.clone(),
        chain.clone(),
        PostgresIndexer::new(pool.clone()),
        config,
        handles,
    ));

    // Several successive heads: each must be trusted (cosignatures stored)
    // and cosigned by this mirror itself, with no cosignature ever served by
    // the author.
    for (index, size) in [10i64, 20, 30].into_iter().enumerate() {
        let sth = sth::sign_tree_head(
            &author,
            "author-1",
            size,
            &hex::encode([index as u8 + 1; 32]),
            &network_id,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        heads.lock().unwrap().insert(size, sth);

        let mut trusted = false;
        for _ in 0..50 {
            let stored = chain
                .list_witness_cosignatures(&network_id, "core", size)
                .await
                .unwrap();
            let by_known = stored
                .iter()
                .filter(|c| c.witness_key_id != own_witness_id)
                .count();
            if by_known >= 2 {
                trusted = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(trusted, "head at size {size} was never trusted");
    }
    worker.abort();

    let own_first = chain
        .list_witness_cosignatures(&network_id, "core", 10)
        .await
        .unwrap();
    assert!(
        own_first.iter().any(|c| c.witness_key_id == own_witness_id),
        "the mirror must cosign an author-valid head independently of majority"
    );
    drop(servers);
}

/// A freshly started mirror: empty database, empty known list (nothing past probation), the
/// witnesses visible only in the peer directory. The head must reach a stored majority through the
/// directory pass alone; the elapsed time is printed. Run against a fresh database: heads of other
/// networks left on shard `core` are refreshed first and can delay this one.
#[tokio::test]
#[ignore = "needs live Postgres"]
async fn a_new_mirror_with_an_empty_known_list_reaches_a_majority_from_the_directory() {
    avalon_devenv::load();
    std::env::set_var("AVALON_ALLOW_PRIVATE_PEERS", "true");
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Postgres must be reachable");

    let network_id = format!("cosign-cold-{}", Uuid::new_v4());
    let author = SigningKey::generate(&mut rand::rng());
    let heads: Heads = Arc::new(Mutex::new(HashMap::new()));
    let author_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ledger/sth/latest"))
        .respond_with(AuthorLatest(heads.clone()))
        .mount(&author_server)
        .await;
    let sth = sth::sign_tree_head(
        &author,
        "author-1",
        10,
        &hex::encode([1u8; 32]),
        &network_id,
        OffsetDateTime::now_utc().replace_nanosecond(0).unwrap(),
    )
    .unwrap();
    heads.lock().unwrap().insert(10, sth);

    let now = OffsetDateTime::now_utc();
    let peers = PeerTable::new();
    let mut servers = Vec::new();
    for _ in 0..3 {
        let key = SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/ledger/sth/\d+$"))
            .respond_with(WitnessAtSize {
                heads: heads.clone(),
                key,
                id: id.clone(),
            })
            .mount(&server)
            .await;
        peers.upsert(PeerInfo {
            identity_bound: false,
            base_url: server.uri(),
            roles: vec!["combined".to_string()],
            protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
            network_id: network_id.clone(),
            last_announced_at: now,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
            connectivity: None,
            witness: Some(WitnessAdvert {
                key_id: id,
                announced_at: now,
                proof: String::new(),
                direct: true,
            }),
        });
        servers.push(server);
    }
    let known_list = KnownListHandle::load_or_new(KnownListConfig::from_env(), None);
    assert!(known_list.confirmed_witness_key_ids().is_empty());

    let own = SigningKey::generate(&mut rand::rng());
    let own_id = hex::encode(own.verifying_key().to_bytes());
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network_id.clone());
    let config = MirrorWatcherConfig {
        peers: vec![("core".to_string(), author_server.uri())],
        poll_interval: Duration::from_millis(300),
        known_shard_ids: BTreeSet::from(["core".to_string()]),
        auto_mirror_discovered: false,
    };
    let handles = MirrorWatcherHandles {
        interest: avalon_server::interest::InterestRegistry::new().0,
        shard_registry: ShardRegistry::new(),
        own_base_url: None,
        wake: Arc::new(tokio::sync::Notify::new()),
        own_shard_id: "mirror-own-shard".to_string(),
        known_list,
        head_gossip: HeadGossipTracker::new(),
        witness: Some(WitnessCosignConfig::new(own, own_id.clone())),
        peers,
        trust_anchors: vec![TrustAnchorEntry {
            label: network_id.clone(),
            network_id: network_id.clone(),
            verify_key: hex::encode(author.verifying_key().to_bytes()),
            signing_key_id: "author-1".to_string(),
            server_url: None,
            environment: NetworkEnvironment::LocalDev,
            seed_nodes: Vec::new(),
            notes: None,
        }],
    };
    let started = std::time::Instant::now();
    let worker = tokio::spawn(run_worker(
        pool.clone(),
        chain.clone(),
        PostgresIndexer::new(pool.clone()),
        config,
        handles,
    ));
    let mut elapsed = None;
    while started.elapsed() < Duration::from_secs(10) {
        let stored = chain
            .list_witness_cosignatures(&network_id, "core", 10)
            .await
            .unwrap();
        if stored.iter().filter(|c| c.witness_key_id != own_id).count() >= 3 {
            elapsed = Some(started.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    worker.abort();
    let elapsed = elapsed.expect("no majority stored from the directory");
    eprintln!("cold mirror: majority stored after {elapsed:?}");
    drop(servers);
}
