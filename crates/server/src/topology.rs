//! `GET /nodes/topology`: everything this node knows about its own place in
//! the network, assembled from its own state only.
//!
//! No aggregator: the response describes this node's view (its active
//! announce/exchange neighbors, the peers it merely heard of, the shards it
//! mirrors) and clients build a graph by walking node to node. Latency
//! figures are measured by this node and labeled with it as the observer.

use std::collections::HashSet;

use avalon_chain::mirror;
use axum::extract::{Query, State};
use axum::http::header::CACHE_CONTROL;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::AppError;
use crate::neighbors::{NeighborSnapshot, RoundTripStats};
use crate::network_coordinates::Coordinate;
use crate::nodes::PeerInfo;
use crate::state::AppState;

const DEFAULT_KNOWN_LIMIT: usize = 100;
const MAX_KNOWN_LIMIT: usize = 500;
const CACHE_CONTROL_VALUE: &str = "public, max-age=5";

#[derive(Debug, Deserialize)]
pub struct TopologyQuery {
    /// Maximum number of `known` entries returned (default 100, capped at 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShardHead {
    pub shard_id: String,
    pub tree_size: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub sth_created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct SelfView {
    pub base_url: Option<String>,
    pub libp2p_peer_id: Option<String>,
    pub protocol_version: String,
    pub network_id: String,
    pub roles: Vec<String>,
    pub stale: bool,
    pub resources: crate::resources::NodeResourceMetrics,
    /// Shards this node authors, with their latest signed tree head.
    pub shards: Vec<ShardHead>,
    /// This node's advisory network coordinate; see `network_coordinates`.
    pub coordinate: Coordinate,
}

/// A round-trip measurement together with the node that took it.
#[derive(Debug, Clone, Serialize)]
pub struct ObservedLatency {
    pub observed_by: Option<String>,
    #[serde(flatten)]
    pub stats: RoundTripStats,
}

#[derive(Debug, Serialize)]
pub struct Neighbor {
    pub base_url: String,
    pub bootstrap: bool,
    /// Empty and `None` below when the peer has not yet appeared in the peer table.
    pub roles: Vec<String>,
    pub protocol_version: Option<String>,
    pub libp2p_peer_id: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_announced_at: Option<OffsetDateTime>,
    /// `None` until an announce to this peer has been attempted.
    pub latency: Option<ObservedLatency>,
    /// The neighbor's own coordinate as it last reported it; advisory.
    pub coordinate: Option<Coordinate>,
}

#[derive(Debug, Serialize)]
pub struct KnownPeer {
    pub base_url: String,
    pub roles: Vec<String>,
    pub protocol_version: String,
    pub libp2p_peer_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub last_announced_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenFinding {
    pub tree_size: i64,
    pub source_a: String,
    pub source_b: String,
}

/// Raw mirror state for one configured source, before lag is derived.
#[derive(Debug, Clone)]
pub struct MirrorInputs {
    pub shard_id: String,
    pub source_url: String,
    pub observed_tree_size: Option<i64>,
    pub last_observed_at: Option<OffsetDateTime>,
    pub mirrored_entries: i64,
    pub last_mirrored_at: Option<OffsetDateTime>,
    pub open_findings: Vec<OpenFinding>,
}

#[derive(Debug, Serialize)]
pub struct MirrorSource {
    pub shard_id: String,
    pub source_url: String,
    /// Tree size of the latest signed tree head observed from the source.
    pub observed_tree_size: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_observed_at: Option<OffsetDateTime>,
    pub mirrored_entries: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_mirrored_at: Option<OffsetDateTime>,
    /// Observed tree size minus mirrored entries, never negative; `None`
    /// until a tree head has been observed from the source.
    pub lag_entries: Option<i64>,
    pub open_equivocations: Vec<OpenFinding>,
}

#[derive(Debug, Serialize)]
pub struct TopologyResponse {
    #[serde(rename = "self")]
    pub self_node: SelfView,
    pub neighbors: Vec<Neighbor>,
    pub known: Vec<KnownPeer>,
    /// Peer table entries that are not active neighbors, before `limit`.
    pub known_total: usize,
    pub mirrors: Vec<MirrorSource>,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
}

pub struct TopologyInputs {
    pub self_view: SelfView,
    pub peers: Vec<PeerInfo>,
    pub neighbors: Vec<NeighborSnapshot>,
    pub mirrors: Vec<MirrorInputs>,
    pub limit: usize,
    pub now: OffsetDateTime,
}

/// Pure assembly of the read model from already-collected state.
pub fn assemble(inputs: TopologyInputs) -> TopologyResponse {
    let TopologyInputs {
        self_view,
        peers,
        neighbors,
        mirrors,
        limit,
        now,
    } = inputs;
    let observer = self_view.base_url.clone();
    let active: HashSet<&str> = neighbors.iter().map(|n| n.base_url.as_str()).collect();

    let mut known: Vec<&PeerInfo> = peers
        .iter()
        .filter(|p| !active.contains(p.base_url.as_str()))
        .collect();
    known.sort_by(|a, b| {
        b.last_announced_at
            .cmp(&a.last_announced_at)
            .then_with(|| a.base_url.cmp(&b.base_url))
    });
    let known_total = known.len();
    let known = known
        .into_iter()
        .take(limit)
        .map(|p| KnownPeer {
            base_url: p.base_url.clone(),
            roles: p.roles.clone(),
            protocol_version: p.protocol_version.clone(),
            libp2p_peer_id: p.libp2p_peer_id.clone(),
            last_announced_at: p.last_announced_at,
        })
        .collect();

    let neighbors = neighbors
        .into_iter()
        .map(|n| {
            let info = peers.iter().find(|p| p.base_url == n.base_url);
            Neighbor {
                bootstrap: n.bootstrap,
                roles: info.map(|p| p.roles.clone()).unwrap_or_default(),
                protocol_version: info.map(|p| p.protocol_version.clone()),
                libp2p_peer_id: info.and_then(|p| p.libp2p_peer_id.clone()),
                last_announced_at: info.map(|p| p.last_announced_at),
                latency: n.round_trip.map(|stats| ObservedLatency {
                    observed_by: observer.clone(),
                    stats,
                }),
                coordinate: n.coordinate,
                base_url: n.base_url,
            }
        })
        .collect();

    let mirrors = mirrors
        .into_iter()
        .map(|m| MirrorSource {
            lag_entries: m
                .observed_tree_size
                .map(|observed| (observed - m.mirrored_entries).max(0)),
            shard_id: m.shard_id,
            source_url: m.source_url,
            observed_tree_size: m.observed_tree_size,
            last_observed_at: m.last_observed_at,
            mirrored_entries: m.mirrored_entries,
            last_mirrored_at: m.last_mirrored_at,
            open_equivocations: m.open_findings,
        })
        .collect();

    TopologyResponse {
        self_node: self_view,
        neighbors,
        known,
        known_total,
        mirrors,
        generated_at: now,
    }
}

async fn collect_mirrors(state: &AppState) -> Result<Vec<MirrorInputs>, AppError> {
    let network_id = state.chain.network_id().to_string();
    let mut out = Vec::new();
    for (shard_id, source_url) in state.shard_mirror_sources.entries() {
        let observed =
            mirror::latest_observed_sth(&state.pool, &network_id, &shard_id, Some(&source_url))
                .await?;
        let progress = mirror::mirrored_progress(&state.pool, &network_id, &shard_id, None).await?;
        let last_mirrored_at =
            mirror::last_mirrored_at(&state.pool, &network_id, &shard_id).await?;
        let findings =
            mirror::unresolved_equivocations(&state.pool, &network_id, &shard_id).await?;
        out.push(MirrorInputs {
            observed_tree_size: observed.as_ref().map(|o| o.tree_size),
            last_observed_at: observed.as_ref().map(|o| o.observed_at),
            mirrored_entries: progress.verified_count,
            last_mirrored_at,
            open_findings: findings
                .into_iter()
                .map(|f| OpenFinding {
                    tree_size: f.tree_size,
                    source_a: f.source_a,
                    source_b: f.source_b,
                })
                .collect(),
            shard_id,
            source_url,
        });
    }
    Ok(out)
}

/// `GET /nodes/topology`: this node's own view of its neighbors, known
/// peers, and mirror sources. Public and read-only.
pub async fn topology(
    State(state): State<AppState>,
    Query(query): Query<TopologyQuery>,
) -> Result<impl IntoResponse, AppError> {
    let status = crate::nodes::build_status(&state);
    let mut shards = Vec::new();
    if let Some(sth) = state.chain.latest_signed_tree_head().await? {
        shards.push(ShardHead {
            shard_id: state.own_shard_id.clone(),
            tree_size: sth.tree_size,
            sth_created_at: sth.created_at,
        });
    }
    let neighbor_table = state.peers.neighbors();
    let self_view = SelfView {
        base_url: state.own_base_url.clone(),
        libp2p_peer_id: neighbor_table.own_libp2p_peer_id(),
        protocol_version: status.protocol_version,
        network_id: status.network_id,
        roles: status.roles,
        stale: status.stale,
        resources: status.resources,
        shards,
        coordinate: neighbor_table.own_coordinate(),
    };
    let response = assemble(TopologyInputs {
        self_view,
        peers: state.peers.list_all(),
        neighbors: neighbor_table.snapshot(),
        mirrors: collect_mirrors(&state).await?,
        limit: query
            .limit
            .unwrap_or(DEFAULT_KNOWN_LIMIT)
            .min(MAX_KNOWN_LIMIT),
        now: OffsetDateTime::now_utc(),
    });
    Ok(([(CACHE_CONTROL, CACHE_CONTROL_VALUE)], Json(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(url: &str, secs: i64) -> PeerInfo {
        PeerInfo {
            base_url: url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "n".to_string(),
            last_announced_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(secs),
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
        }
    }

    fn self_view() -> SelfView {
        SelfView {
            base_url: Some("http://me".to_string()),
            libp2p_peer_id: None,
            protocol_version: "0.1.0".to_string(),
            network_id: "n".to_string(),
            roles: vec![],
            stale: false,
            resources: Default::default(),
            shards: vec![],
            coordinate: Default::default(),
        }
    }

    fn inputs(
        peers: Vec<PeerInfo>,
        neighbors: Vec<NeighborSnapshot>,
        limit: usize,
    ) -> TopologyInputs {
        TopologyInputs {
            self_view: self_view(),
            peers,
            neighbors,
            mirrors: vec![],
            limit,
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn snap(url: &str, bootstrap: bool) -> NeighborSnapshot {
        NeighborSnapshot {
            base_url: url.to_string(),
            bootstrap,
            round_trip: None,
            coordinate: None,
        }
    }

    #[test]
    fn active_and_known_are_split_and_never_overlap() {
        let r = assemble(inputs(
            vec![
                peer("http://a", 1),
                peer("http://b", 2),
                peer("http://c", 3),
            ],
            vec![snap("http://a", true)],
            10,
        ));
        assert_eq!(r.neighbors.len(), 1);
        assert_eq!(r.neighbors[0].base_url, "http://a");
        assert!(r.neighbors[0].bootstrap);
        assert_eq!(r.neighbors[0].roles, vec!["combined".to_string()]);
        let known: Vec<_> = r.known.iter().map(|k| k.base_url.as_str()).collect();
        assert_eq!(known, ["http://c", "http://b"]);
        assert_eq!(r.known_total, 2);
    }

    #[test]
    fn known_is_limited_newest_first_but_total_is_reported() {
        let peers = (0..5).map(|i| peer(&format!("http://p{i}"), i)).collect();
        let r = assemble(inputs(peers, vec![], 2));
        assert_eq!(r.known.len(), 2);
        assert_eq!(r.known[0].base_url, "http://p4");
        assert_eq!(r.known_total, 5);
    }

    #[test]
    fn active_neighbor_absent_from_peer_table_is_still_listed() {
        let r = assemble(inputs(vec![], vec![snap("http://boot", true)], 10));
        assert_eq!(r.neighbors.len(), 1);
        assert!(r.neighbors[0].roles.is_empty());
        assert!(r.neighbors[0].protocol_version.is_none());
        assert!(r.neighbors[0].latency.is_none());
    }

    #[test]
    fn latency_is_labeled_with_the_observer() {
        let table = crate::neighbors::NeighborTable::new();
        table.set_active(&["http://a".to_string()], &[]);
        table.record_success("http://a", std::time::Duration::from_millis(7));
        let r = assemble(inputs(vec![peer("http://a", 1)], table.snapshot(), 10));
        let json = serde_json::to_value(&r).unwrap();
        let lat = &json["neighbors"][0]["latency"];
        assert_eq!(lat["observed_by"], "http://me");
        assert_eq!(lat["measurement"], "application_round_trip");
        assert!(lat["last_ms"].as_f64().unwrap() > 0.0);
        assert!(json.get("self").is_some());
    }

    #[test]
    fn mirror_lag_is_observed_minus_mirrored_and_never_negative() {
        let mk = |observed: Option<i64>, mirrored: i64| MirrorInputs {
            shard_id: "core".to_string(),
            source_url: "http://src".to_string(),
            observed_tree_size: observed,
            last_observed_at: None,
            mirrored_entries: mirrored,
            last_mirrored_at: None,
            open_findings: vec![],
        };
        let mut i = inputs(vec![], vec![], 10);
        i.mirrors = vec![mk(Some(100), 40), mk(Some(10), 12), mk(None, 0)];
        let r = assemble(i);
        let lags: Vec<_> = r.mirrors.iter().map(|m| m.lag_entries).collect();
        assert_eq!(lags, [Some(60), Some(0), None]);
    }

    #[test]
    fn open_findings_are_carried_through() {
        let mut i = inputs(vec![], vec![], 10);
        i.mirrors = vec![MirrorInputs {
            shard_id: "core".to_string(),
            source_url: "http://src".to_string(),
            observed_tree_size: Some(5),
            last_observed_at: None,
            mirrored_entries: 5,
            last_mirrored_at: None,
            open_findings: vec![OpenFinding {
                tree_size: 5,
                source_a: "http://a".to_string(),
                source_b: "http://b".to_string(),
            }],
        }];
        let r = assemble(i);
        assert_eq!(r.mirrors[0].open_equivocations.len(), 1);
    }
}
