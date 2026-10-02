//! Push notification for mirror sync — the delivery half of
//! push-based mirror sync, built entirely on top of `crate::interest`'s
//! already-existing DHT-backed registration rather than
//! a new addressing mechanism. See `crate::interest`'s own module doc for
//! why `InterestScope::Network` reuses that exact registration/lookup
//! path instead of inventing a second one.
//!
//! **Delivery transport: direct HTTP, not a libp2p stream.** Once
//! [`notify_peers`] resolves the interested set via `interest::lookup`,
//! it POSTs a small notification straight to each peer's known HTTP base
//! URL (the same `base_url` identity `crate::nodes`' peer table and
//! `mirror_watcher`'s own polling already use) rather than opening a
//! direct libp2p stream. Chosen because every piece an HTTP push needs —
//! a `reqwest::Client`, the peer's reachable base URL, an axum route to
//! receive it — already exists in this codebase for the exact same shape
//! of problem, while a libp2p stream would require adding a brand-new
//! request-response protocol to `crate::dht`'s swarm (today only
//! Kademlia put/get plus `identify` — see that module's own doc comment)
//! for a one-shot, fire-and-forget notification that never needs a
//! persistent bidirectional connection. The DHT is still what makes the
//! *addressing* targeted rather than a broadcast; only the last hop is
//! plain HTTP.
//!
//! **Never trusted content.** A receiving node's `POST /mirror/notify`
//! handler ([`notify`]) acts only for one of its own configured mirror
//! sources, on its own network, announcing a tree size beyond what it has
//! observed from that source; even then it only wakes
//! `mirror_watcher::run_worker`'s loop early. Every actual verification step
//! is exactly `mirror_watcher`'s existing signature/inclusion-proof
//! pipeline, run on this node's own initiative against its own
//! configured peers — a push notification is a prompt to look, never
//! something looked at directly, and a non-source's never causes any work.
//!
//! **Additive to tier 1.** A node with `AVALON_DHT_ENABLED` unset (or one
//! nobody has registered interest for) gets no pushes at all and mirrors
//! exactly as it always has, on poll alone — see
//! `mirror_watcher`'s own module doc for the two-tier design this
//! implements.

use serde::{Deserialize, Serialize};

use crate::dht::DhtCommandSender;
use crate::interest::{self, InterestScope, RedisFastPath};
use crate::state::AppState;

/// Bundles what [`notify_peers`] needs to resolve interested peers and
/// reach them — built once at startup (`main.rs`) alongside the other
/// DHT-dependent workers. Never constructed at all when
/// `AVALON_DHT_ENABLED` is unset (`dht_commands` has nothing to hand it),
/// in which case `main.rs` simply never spawns anything that would call
/// [`notify_peers`] — tier 1 (permissionless polling) is completely
/// unaffected either way, per this ticket's own invariant.
#[derive(Clone)]
pub struct MirrorPushConfig {
    dht_commands: DhtCommandSender,
    redis_fast_path: Option<RedisFastPath>,
    own_base_url: Option<String>,
    client: crate::node_http::NodeClient,
}

impl MirrorPushConfig {
    pub fn new(
        dht_commands: DhtCommandSender,
        redis_fast_path: Option<RedisFastPath>,
        own_base_url: Option<String>,
    ) -> Self {
        Self {
            dht_commands,
            redis_fast_path,
            own_base_url,
            client: crate::node_http::NodeClient::peer(),
        }
    }
}

/// The wire body for `POST /mirror/notify` — deliberately just enough to
/// let a receiving node log what prompted the wake-up and decide whether
/// it's even worth re-polling early (a `tree_size` at or below what this
/// node already has would still just fall through to its normal
/// verify/corroborate pass, never trusted directly). See this module's own
/// doc comment for why nothing here is ever trusted as settled fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorNotifyRequest {
    pub network_id: String,
    pub tree_size: i64,
}

/// Looks up every peer currently registered as interested in `network_id`
/// (via `crate::interest::lookup`, reusing #585's Redis fast-path when
/// configured) and POSTs a best-effort [`MirrorNotifyRequest`] to each,
/// excluding this node's own `own_base_url` (a node that both authors and
/// mirrors the same network would otherwise notify itself). Every
/// individual delivery is fire-and-forget and logged, never propagated as
/// an error — a peer that's unreachable or slow to answer is no worse off
/// than it already was pre-#596: it just falls back to its own poll tick,
/// which is exactly the correctness backstop this design leans on.
pub async fn notify_peers(config: &MirrorPushConfig, network_id: &str, tree_size: i64) {
    let scope = InterestScope::for_network(network_id);
    let peers =
        interest::lookup(&config.dht_commands, scope, config.redis_fast_path.as_ref()).await;
    if peers.is_empty() {
        return;
    }

    let body = MirrorNotifyRequest {
        network_id: network_id.to_string(),
        tree_size,
    };
    for peer in peers {
        if config.own_base_url.as_deref() == Some(peer.as_str()) {
            continue;
        }
        let client = config.client.clone();
        let peer_url = peer.clone();
        let body = body.clone();
        tokio::spawn(async move {
            match client
                .post(format!("{peer_url}/mirror/notify"))
                .json(&body)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    tracing::debug!(
                        peer = %peer_url,
                        network_id = %body.network_id,
                        tree_size = body.tree_size,
                        "mirror-push: notified peer of a new STH"
                    );
                }
                Ok(response) => {
                    tracing::warn!(
                        peer = %peer_url,
                        status = %response.status(),
                        "mirror-push: peer rejected push notification — its own poll fallback still applies"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        peer = %peer_url,
                        error = %err,
                        "mirror-push: failed to reach peer — its own poll fallback still applies"
                    );
                }
            }
        });
    }
}

/// The configured `(shard, url)` mirror sources that `signer` is: peers it is bound to in the
/// peer table whose URL names a source. Empty means `signer` is not one of this node's sources.
pub fn sources_of_signer(
    peers: &crate::nodes::PeerTable,
    sources: &crate::settlement::ShardMirrorSources,
    signer: &libp2p::PeerId,
) -> Vec<(String, String)> {
    let id = signer.to_string();
    let origins: Vec<String> = peers
        .list_all()
        .iter()
        .filter(|p| p.identity_bound && p.libp2p_peer_id.as_deref() == Some(id.as_str()))
        .filter_map(|p| crate::node_auth::normalized_origin(&p.base_url))
        .collect();
    sources
        .entries()
        .into_iter()
        .filter(|(_, url)| {
            crate::node_auth::normalized_origin(url).is_some_and(|o| origins.contains(&o))
        })
        .collect()
}

/// Whether `tree_size` is beyond what this node has observed from any of `sources` for
/// `network_id`, i.e. whether polling again could find anything new.
pub async fn exceeds_observed(
    pool: &sqlx::PgPool,
    sources: &[(String, String)],
    network_id: &str,
    tree_size: i64,
) -> Result<bool, avalon_chain::SettlementError> {
    for (shard_id, url) in sources {
        let observed =
            avalon_chain::mirror::latest_observed_tree_size(pool, url, network_id, shard_id)
                .await?;
        if tree_size > observed {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `POST /mirror/notify` — see this module's own doc comment for the full "never trusted
/// content" invariant. The signer must be one of this node's configured mirror sources (403),
/// the network id must be this node's (400), and the announced tree size must exceed what this
/// node has already observed from that source; only then is the mirror watcher woken. A stale
/// size is acknowledged (202) without waking it. A source matches by URL origin, so one
/// configured by hostname but announced by IP (or the reverse) is refused and polling covers it. Nothing here starts outbound work itself.
pub async fn notify(
    axum::extract::State(state): axum::extract::State<AppState>,
    crate::node_auth::AuthenticatedNode(signer): crate::node_auth::AuthenticatedNode,
    axum::Json(body): axum::Json<MirrorNotifyRequest>,
) -> axum::http::StatusCode {
    use axum::http::StatusCode;
    let sources = sources_of_signer(&state.peers, &state.shard_mirror_sources, &signer);
    if sources.is_empty() {
        return StatusCode::FORBIDDEN;
    }
    if body.network_id != state.chain.network_id() || body.tree_size <= 0 {
        return StatusCode::BAD_REQUEST;
    }
    match exceeds_observed(&state.pool, &sources, &body.network_id, body.tree_size).await {
        Ok(true) => {
            tracing::info!(
                network_id = %body.network_id,
                tree_size = body.tree_size,
                "mirror-push: received a push notification — waking the mirror-watcher early"
            );
            state.mirror_wake.notify_one();
            StatusCode::ACCEPTED
        }
        Ok(false) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_request_round_trips_through_json() {
        let body = MirrorNotifyRequest {
            network_id: "avalon-dev-local".to_string(),
            tree_size: 42,
        };
        let json = serde_json::to_string(&body).expect("serialize");
        let decoded: MirrorNotifyRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.network_id, body.network_id);
        assert_eq!(decoded.tree_size, body.tree_size);
    }

    fn entry(id: &libp2p::PeerId, url: &str, bound: bool) -> crate::nodes::PeerInfo {
        crate::nodes::PeerInfo {
            base_url: url.into(),
            roles: vec![],
            protocol_version: "0.1.0".into(),
            network_id: "n".into(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: Some(id.to_string()),
            libp2p_listen_addrs: vec![],
            witness: None,
            connectivity: None,
            identity_bound: bound,
        }
    }

    #[test]
    fn only_a_bound_peer_at_a_configured_source_url_is_a_source() {
        let (src, other, unbound) = (
            libp2p::PeerId::random(),
            libp2p::PeerId::random(),
            libp2p::PeerId::random(),
        );
        let peers = crate::nodes::PeerTable::new();
        peers.upsert(entry(&src, "http://Src.test:80/", true));
        peers.upsert(entry(&other, "http://other.test", true));
        peers.upsert(entry(&unbound, "http://unbound.test", false));
        let sources = crate::settlement::ShardMirrorSources::from_raw(
            "http://src.test,aux=http://unbound.test",
        );
        assert_eq!(
            sources_of_signer(&peers, &sources, &src),
            vec![("core".to_string(), "http://src.test".to_string())]
        );
        assert!(sources_of_signer(&peers, &sources, &other).is_empty());
        assert!(sources_of_signer(&peers, &sources, &unbound).is_empty());
        assert!(sources_of_signer(&peers, &sources, &libp2p::PeerId::random()).is_empty());
    }

    #[test]
    fn a_source_matches_only_on_the_same_origin() {
        let peers = crate::nodes::PeerTable::new();
        let source_of = |announced: &str, configured: &str| {
            let id = libp2p::PeerId::random();
            let peers = crate::nodes::PeerTable::new();
            peers.upsert(entry(&id, announced, true));
            let sources = crate::settlement::ShardMirrorSources::from_raw(configured);
            !sources_of_signer(&peers, &sources, &id).is_empty()
        };
        drop(peers);
        // Userinfo and a default port are not part of the origin.
        assert!(source_of(
            "http://user:pw@src.test:8080",
            "http://src.test:8080"
        ));
        assert!(source_of("http://src.test:80", "http://src.test"));
        assert!(source_of("https://src.test:443", "https://src.test"));
        // The scheme, the port and the host spelling all have to agree.
        assert!(!source_of("https://src.test", "http://src.test"));
        assert!(!source_of("http://src.test:8081", "http://src.test:8080"));
        assert!(!source_of("http://src.test:8080", "http://src.test"));
        assert!(!source_of("http://192.0.2.7:8080", "http://src.test:8080"));
        assert!(!source_of("http://src.test:8080", "http://192.0.2.7:8080"));
        // IPv6 hosts compare in their normalized form.
        assert!(source_of(
            "http://[::1]:8080",
            "http://[0:0:0:0:0:0:0:1]:8080"
        ));
        assert!(!source_of("http://[::1]:8080", "http://[::2]:8080"));
        // A p2p entry names no origin, so it is never a source.
        assert!(!source_of("p2p://somepeer", "http://src.test"));
    }
}
