//! Overlay next-hop selection: given a target, which active neighbor a node forwards to.
//!
//! Pure and I/O-free. The rules, in order:
//! 1. The target is an active neighbor: the next hop is the target itself.
//! 2. Otherwise the unvisited active neighbor with the smallest XOR distance to the target,
//!    and only if that distance is strictly smaller than this node's own distance.
//! 3. Otherwise [`NextHop::NoRoute`] with the reason.
//!
//! Only nodes on the caller's own `network_id` are ever selected.
//!
//! Key derivation: when this node, the target and every candidate carry a parseable libp2p
//! peer id, a node's key is `SHA-256(PeerId::to_bytes())`, the same key libp2p Kademlia uses
//! for its XOR metric. If any of them lacks one, every node in that decision falls back to
//! `SHA-256` of its canonical `base_url` (trimmed, trailing slashes removed, ASCII-lowercased),
//! so all distances in one decision are always measured in a single key space.

use std::collections::HashSet;

use libp2p::PeerId;
use sha2::{Digest, Sha256};

/// A node as the routing rule sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayNode {
    pub base_url: String,
    pub network_id: String,
    pub libp2p_peer_id: Option<String>,
}

/// Why no next hop exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRouteReason {
    /// The target is this node.
    TargetIsSelf,
    /// The target is on a different network id than this node.
    ForeignNetwork,
    /// There are no active neighbors on this node's network.
    NoNeighbors,
    /// Every neighbor closer to the target than this node has already been visited.
    AllVisited,
    /// No unvisited neighbor is strictly closer to the target than this node.
    NoProgress,
}

/// The decision for one hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextHop {
    /// The target is an active neighbor.
    Direct(OverlayNode),
    /// A neighbor strictly closer to the target.
    Forward(OverlayNode),
    NoRoute(NoRouteReason),
}

impl From<NoRouteReason> for NextHop {
    fn from(r: NoRouteReason) -> Self {
        NextHop::NoRoute(r)
    }
}

/// Canonical form of a base URL used for identity, visited-set membership and key fallback.
pub fn canonical_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn peer_id_key(node: &OverlayNode) -> Option<[u8; 32]> {
    let id: PeerId = node.libp2p_peer_id.as_ref()?.parse().ok()?;
    Some(Sha256::digest(id.to_bytes()).into())
}

fn url_key(node: &OverlayNode) -> [u8; 32] {
    Sha256::digest(canonical_base_url(&node.base_url).as_bytes()).into()
}

fn xor(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// Chooses the next hop from `me` toward `target`. `visited` holds canonical base URLs
/// (see [`canonical_base_url`]) of nodes the request already passed through.
pub fn next_hop(
    me: &OverlayNode,
    target: &OverlayNode,
    active_neighbors: &[OverlayNode],
    visited: &HashSet<String>,
) -> NextHop {
    let my_url = canonical_base_url(&me.base_url);
    let target_url = canonical_base_url(&target.base_url);
    if target_url == my_url {
        return NoRouteReason::TargetIsSelf.into();
    }
    if target.network_id != me.network_id {
        return NoRouteReason::ForeignNetwork.into();
    }
    let neighbors: Vec<(String, &OverlayNode)> = active_neighbors
        .iter()
        .filter(|n| n.network_id == me.network_id)
        .map(|n| (canonical_base_url(&n.base_url), n))
        .filter(|(url, _)| *url != my_url)
        .collect();
    if let Some((_, hit)) = neighbors.iter().find(|(url, _)| *url == target_url) {
        return NextHop::Direct((*hit).clone());
    }
    if neighbors.is_empty() {
        return NoRouteReason::NoNeighbors.into();
    }

    let use_peer_ids = peer_id_key(me).is_some()
        && peer_id_key(target).is_some()
        && neighbors.iter().all(|(_, n)| peer_id_key(n).is_some());
    let key = |n: &OverlayNode| {
        if use_peer_ids {
            peer_id_key(n).expect("checked above")
        } else {
            url_key(n)
        }
    };
    let target_key = key(target);
    let my_distance = xor(&key(me), &target_key);

    let mut best: Option<([u8; 32], &str, &OverlayNode)> = None;
    let mut closer_but_visited = false;
    for (url, n) in &neighbors {
        let d = xor(&key(n), &target_key);
        if d >= my_distance {
            continue;
        }
        if visited.contains(url) {
            closer_but_visited = true;
            continue;
        }
        let better = match &best {
            None => true,
            Some((bd, burl, _)) => d < *bd || (d == *bd && url.as_str() < *burl),
        };
        if better {
            best = Some((d, url.as_str(), n));
        }
    }
    match best {
        Some((_, _, n)) => NextHop::Forward(n.clone()),
        None if closer_but_visited => NoRouteReason::AllVisited.into(),
        None => NoRouteReason::NoProgress.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn node(url: &str) -> OverlayNode {
        OverlayNode {
            base_url: url.into(),
            network_id: "net".into(),
            libp2p_peer_id: None,
        }
    }

    fn with_peer_id(url: &str) -> OverlayNode {
        let id = PeerId::from(libp2p::identity::Keypair::generate_ed25519().public());
        OverlayNode {
            libp2p_peer_id: Some(id.to_string()),
            ..node(url)
        }
    }

    fn dist(a: &OverlayNode, b: &OverlayNode) -> [u8; 32] {
        xor(&url_key(a), &url_key(b))
    }

    fn strip(n: &OverlayNode) -> OverlayNode {
        OverlayNode {
            libp2p_peer_id: None,
            ..n.clone()
        }
    }

    #[test]
    fn direct_hit_when_target_is_neighbor() {
        let (me, t, o) = (node("http://a"), node("http://t"), node("http://o"));
        let r = next_hop(&me, &t, &[o, t.clone()], &HashSet::new());
        assert_eq!(r, NextHop::Direct(t));
    }

    #[test]
    fn direct_hit_ignores_visited_and_progress() {
        let (me, t) = (node("http://a"), node("http://t"));
        let visited = HashSet::from([canonical_base_url("http://t")]);
        assert_eq!(
            next_hop(&me, &t, std::slice::from_ref(&t), &visited),
            NextHop::Direct(t)
        );
    }

    #[test]
    fn target_is_self() {
        let me = node("http://a");
        assert_eq!(
            next_hop(
                &me,
                &node("http://A/"),
                &[node("http://b")],
                &HashSet::new()
            ),
            NextHop::NoRoute(NoRouteReason::TargetIsSelf)
        );
    }

    #[test]
    fn no_neighbors() {
        assert_eq!(
            next_hop(&node("http://a"), &node("http://t"), &[], &HashSet::new()),
            NextHop::NoRoute(NoRouteReason::NoNeighbors)
        );
    }

    #[test]
    fn picks_closest_strictly_closer_neighbor() {
        let me = node("http://me");
        let t = node("http://target");
        let neighbors: Vec<_> = (0..40).map(|i| node(&format!("http://n{i}"))).collect();
        let best = neighbors.iter().min_by_key(|n| dist(n, &t)).unwrap();
        match next_hop(&me, &t, &neighbors, &HashSet::new()) {
            NextHop::Forward(n) => {
                assert_eq!(&n, best);
                assert!(dist(&n, &t) < dist(&me, &t));
            }
            NextHop::NoRoute(NoRouteReason::NoProgress) => assert!(dist(best, &t) >= dist(&me, &t)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn no_progress_when_all_neighbors_farther() {
        let t = node("http://target");
        let neighbors: Vec<_> = (0..40).map(|i| node(&format!("http://n{i}"))).collect();
        let me = neighbors
            .iter()
            .min_by_key(|n| dist(n, &t))
            .unwrap()
            .clone();
        let others: Vec<_> = neighbors.into_iter().filter(|n| *n != me).collect();
        assert_eq!(
            next_hop(&me, &t, &others, &HashSet::new()),
            NextHop::NoRoute(NoRouteReason::NoProgress)
        );
    }

    #[test]
    fn visited_neighbors_are_skipped() {
        let t = node("http://target");
        let mut all: Vec<_> = (0..40).map(|i| node(&format!("http://n{i}"))).collect();
        all.sort_by_key(|n| dist(n, &t));
        let me = all.pop().unwrap();
        let mut visited = HashSet::new();
        assert_eq!(
            next_hop(&me, &t, &all, &visited),
            NextHop::Forward(all[0].clone())
        );
        visited.insert(canonical_base_url(&all[0].base_url));
        assert_eq!(
            next_hop(&me, &t, &all, &visited),
            NextHop::Forward(all[1].clone())
        );
        for n in &all {
            visited.insert(canonical_base_url(&n.base_url));
        }
        assert_eq!(
            next_hop(&me, &t, &all, &visited),
            NextHop::NoRoute(NoRouteReason::AllVisited)
        );
    }

    #[test]
    fn foreign_network_is_never_selected() {
        let t = node("http://target");
        let mut all: Vec<_> = (0..20).map(|i| node(&format!("http://n{i}"))).collect();
        all.sort_by_key(|n| dist(n, &t));
        let me = all.pop().unwrap();
        all[0].network_id = "other".into();
        assert_eq!(
            next_hop(&me, &t, &all, &HashSet::new()),
            NextHop::Forward(all[1].clone())
        );
        let mut ft = t.clone();
        ft.network_id = "other".into();
        assert_eq!(
            next_hop(&me, &ft, &all, &HashSet::new()),
            NextHop::NoRoute(NoRouteReason::ForeignNetwork)
        );
        let mut foreign_target = all[0].clone();
        foreign_target.network_id = "net".into();
        // A same-URL neighbor on another network is not a direct hit.
        assert!(!matches!(
            next_hop(&me, &foreign_target, &all, &HashSet::new()),
            NextHop::Direct(_)
        ));
    }

    #[test]
    fn deterministic_regardless_of_neighbor_order() {
        let (me, t) = (node("http://me"), node("http://target"));
        let mut ns: Vec<_> = (0..30).map(|i| node(&format!("http://n{i}"))).collect();
        let a = next_hop(&me, &t, &ns, &HashSet::new());
        ns.reverse();
        assert_eq!(a, next_hop(&me, &t, &ns, &HashSet::new()));
    }

    #[test]
    fn url_canonicalization_is_case_and_slash_insensitive() {
        assert_eq!(
            canonical_base_url(" HTTP://Host:8080// "),
            "http://host:8080"
        );
        assert_eq!(url_key(&node("http://X/")), url_key(&node("http://x")));
    }

    #[test]
    fn peer_id_key_matches_kademlia_key() {
        let n = with_peer_id("http://a");
        let id: PeerId = n.libp2p_peer_id.as_ref().unwrap().parse().unwrap();
        let kad = libp2p::kad::KBucketKey::from(id);
        assert_eq!(kad.hashed_bytes(), &peer_id_key(&n).unwrap()[..]);
    }

    #[test]
    fn peer_id_keys_drive_selection_when_all_present() {
        let me = with_peer_id("http://me");
        let t = with_peer_id("http://target");
        let ns: Vec<_> = (0..30)
            .map(|i| with_peer_id(&format!("http://n{i}")))
            .collect();
        let tk = peer_id_key(&t).unwrap();
        let my_d = xor(&peer_id_key(&me).unwrap(), &tk);
        let best = ns
            .iter()
            .min_by_key(|n| xor(&peer_id_key(n).unwrap(), &tk))
            .unwrap();
        let expected = if xor(&peer_id_key(best).unwrap(), &tk) < my_d {
            NextHop::Forward(best.clone())
        } else {
            NextHop::NoRoute(NoRouteReason::NoProgress)
        };
        assert_eq!(next_hop(&me, &t, &ns, &HashSet::new()), expected);
    }

    #[test]
    fn falls_back_to_url_keys_when_any_peer_id_missing() {
        let me = with_peer_id("http://me");
        let t = with_peer_id("http://target");
        let mut ns: Vec<_> = (0..20)
            .map(|i| with_peer_id(&format!("http://n{i}")))
            .collect();
        ns[0].libp2p_peer_id = None;
        let mixed = next_hop(&me, &t, &ns, &HashSet::new());
        let stripped: Vec<_> = ns.iter().map(strip).collect();
        let expected = next_hop(&strip(&me), &strip(&t), &stripped, &HashSet::new());
        let strip_hop = |h: NextHop| match h {
            NextHop::Forward(n) => NextHop::Forward(strip(&n)),
            NextHop::Direct(n) => NextHop::Direct(strip(&n)),
            x => x,
        };
        assert_eq!(strip_hop(mixed), expected);
    }

    #[test]
    fn unparseable_peer_id_counts_as_missing() {
        let mut me = node("http://me");
        me.libp2p_peer_id = Some("not-a-peer-id".into());
        assert!(peer_id_key(&me).is_none());
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Greedy routing over random bounded-degree overlays terminates, never revisits a
    /// node, and either reaches the target or reports NoRoute.
    #[test]
    fn greedy_routing_reaches_target_or_reports_no_route_without_revisits() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut reached, mut stuck) = (0, 0);
        for _ in 0..200 {
            let n = 5 + rng.below(60);
            let max_degree = 3 + rng.below(4);
            let nodes: Vec<_> = (0..n)
                .map(|i| node(&format!("http://node{i}.example")))
                .collect();
            let mut adj: HashMap<usize, HashSet<usize>> =
                (0..n).map(|i| (i, HashSet::new())).collect();
            // A ring keeps the overlay connected; random chords stay within the degree bound.
            for i in 0..n {
                let j = (i + 1) % n;
                adj.get_mut(&i).unwrap().insert(j);
                adj.get_mut(&j).unwrap().insert(i);
            }
            for _ in 0..n * 2 {
                let (a, b) = (rng.below(n), rng.below(n));
                if a != b && adj[&a].len() < max_degree && adj[&b].len() < max_degree {
                    adj.get_mut(&a).unwrap().insert(b);
                    adj.get_mut(&b).unwrap().insert(a);
                }
            }
            for _ in 0..10 {
                let (src, dst) = (rng.below(n), rng.below(n));
                if src == dst {
                    continue;
                }
                let mut cur = src;
                let mut visited = HashSet::from([canonical_base_url(&nodes[cur].base_url)]);
                let mut path = vec![cur];
                loop {
                    assert!(path.len() <= n, "route longer than node count");
                    let neighbors: Vec<_> = adj[&cur].iter().map(|&i| nodes[i].clone()).collect();
                    match next_hop(&nodes[cur], &nodes[dst], &neighbors, &visited) {
                        NextHop::Direct(t) => {
                            assert_eq!(t, nodes[dst]);
                            reached += 1;
                            break;
                        }
                        NextHop::Forward(nx) => {
                            let idx = nodes.iter().position(|x| *x == nx).unwrap();
                            assert!(adj[&cur].contains(&idx));
                            assert!(!path.contains(&idx), "revisited a node");
                            assert!(dist(&nx, &nodes[dst]) < dist(&nodes[cur], &nodes[dst]));
                            visited.insert(canonical_base_url(&nx.base_url));
                            path.push(idx);
                            cur = idx;
                        }
                        NextHop::NoRoute(_) => {
                            stuck += 1;
                            break;
                        }
                    }
                }
            }
        }
        assert!(reached > 0 && stuck > 0, "reached={reached} stuck={stuck}");
    }
}
