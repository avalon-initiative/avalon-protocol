//! Overlay next-hop selection: given a target, which active neighbor a node forwards to.
//!
//! Pure and I/O-free. The rules, in order:
//! 1. The target is an active neighbor: the next hop is the target itself.
//! 2. Otherwise an ordered list of unvisited active neighbors: those strictly closer to the
//!    target than this node first, then the rest, each tier by ascending XOR distance with
//!    recently failing neighbors after healthy ones.
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
    /// Every neighbor has already been visited.
    AllVisited,
}

/// The decision for one hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextHop {
    /// The target is an active neighbor; `fallbacks` are the other candidates in order, for
    /// when the target cannot be reached directly.
    Direct {
        target: OverlayNode,
        fallbacks: Vec<OverlayNode>,
    },
    /// Unvisited neighbors to try in order; never empty.
    Forward(Vec<OverlayNode>),
    NoRoute(NoRouteReason),
}

impl From<NoRouteReason> for NextHop {
    fn from(r: NoRouteReason) -> Self {
        NextHop::NoRoute(r)
    }
}

/// Canonical form of a base URL used for identity, visited-set membership and key fallback.
pub fn canonical_base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    // Peer ids are case-sensitive base58.
    if crate::node_http::is_p2p_url(url) {
        return url.to_string();
    }
    url.to_ascii_lowercase()
}

fn peer_id_key(node: &OverlayNode) -> Option<[u8; 32]> {
    let id: PeerId = node.libp2p_peer_id.as_ref()?.parse().ok()?;
    Some(Sha256::digest(id.to_bytes()).into())
}

fn url_key(node: &OverlayNode) -> [u8; 32] {
    Sha256::digest(canonical_base_url(&node.base_url).as_bytes()).into()
}

#[cfg(test)]
pub(crate) fn url_distance(a: &OverlayNode, b: &OverlayNode) -> [u8; 32] {
    xor(&url_key(a), &url_key(b))
}

fn xor(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// Sort key: not-closer tier, failing, distance, URL (for a deterministic order).
type Rank<'a> = (bool, bool, [u8; 32], &'a str);

/// Chooses the next hops from `me` toward `target`. `visited` and `failing` hold canonical
/// base URLs (see [`canonical_base_url`]): nodes the request already passed through, and
/// neighbors with recent failed contact.
pub fn next_hop(
    me: &OverlayNode,
    target: &OverlayNode,
    active_neighbors: &[OverlayNode],
    visited: &HashSet<String>,
    failing: &HashSet<String>,
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
    if neighbors.is_empty() {
        return NoRouteReason::NoNeighbors.into();
    }
    let direct = neighbors
        .iter()
        .find(|(url, _)| *url == target_url)
        .map(|(_, n)| (*n).clone());

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

    let mut ranked: Vec<(Rank, &OverlayNode)> = neighbors
        .iter()
        .filter(|(url, _)| *url != target_url && !visited.contains(url))
        .map(|(url, n)| {
            let d = xor(&key(n), &target_key);
            let rank = (d >= my_distance, failing.contains(url), d, url.as_str());
            (rank, *n)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    let ordered: Vec<OverlayNode> = ranked.into_iter().map(|(_, n)| n.clone()).collect();
    match direct {
        Some(target) => NextHop::Direct {
            target,
            fallbacks: ordered,
        },
        None if ordered.is_empty() => NoRouteReason::AllVisited.into(),
        None => NextHop::Forward(ordered),
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

    fn route(
        me: &OverlayNode,
        t: &OverlayNode,
        ns: &[OverlayNode],
        visited: &HashSet<String>,
    ) -> NextHop {
        next_hop(me, t, ns, visited, &HashSet::new())
    }

    fn forward(h: NextHop) -> Vec<OverlayNode> {
        match h {
            NextHop::Forward(v) => v,
            other => panic!("expected Forward, got {other:?}"),
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
        let r = route(&me, &t, &[o.clone(), t.clone()], &HashSet::new());
        assert_eq!(
            r,
            NextHop::Direct {
                target: t,
                fallbacks: vec![o]
            }
        );
    }

    #[test]
    fn direct_hit_ignores_visited_and_progress() {
        let (me, t) = (node("http://a"), node("http://t"));
        let visited = HashSet::from([canonical_base_url("http://t")]);
        assert_eq!(
            route(&me, &t, std::slice::from_ref(&t), &visited),
            NextHop::Direct {
                target: t,
                fallbacks: vec![]
            }
        );
    }

    #[test]
    fn target_is_self() {
        let me = node("http://a");
        assert_eq!(
            route(
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
            route(&node("http://a"), &node("http://t"), &[], &HashSet::new()),
            NextHop::NoRoute(NoRouteReason::NoNeighbors)
        );
    }

    fn sorted_by_distance(t: &OverlayNode, n: usize) -> Vec<OverlayNode> {
        let mut all: Vec<_> = (0..n).map(|i| node(&format!("http://n{i}"))).collect();
        all.sort_by_key(|x| dist(x, t));
        all
    }

    #[test]
    fn closer_neighbors_come_first_by_distance_then_the_rest() {
        let me = node("http://me");
        let t = node("http://target");
        let neighbors: Vec<_> = (0..40).map(|i| node(&format!("http://n{i}"))).collect();
        let order = forward(route(&me, &t, &neighbors, &HashSet::new()));
        assert_eq!(order.len(), neighbors.len());
        let my_d = dist(&me, &t);
        let closer = order.iter().take_while(|n| dist(n, &t) < my_d).count();
        assert!(order[closer..].iter().all(|n| dist(n, &t) >= my_d));
        for tier in [&order[..closer], &order[closer..]] {
            assert!(tier.windows(2).all(|w| dist(&w[0], &t) <= dist(&w[1], &t)));
        }
    }

    #[test]
    fn a_node_with_no_closer_neighbor_still_forwards() {
        let t = node("http://target");
        let mut all = sorted_by_distance(&t, 20);
        let me = all.remove(0);
        let order = forward(route(&me, &t, &all, &HashSet::new()));
        assert_eq!(order, all);
    }

    #[test]
    fn a_single_neighbor_is_a_candidate_even_when_farther() {
        let t = node("http://target");
        let mut all = sorted_by_distance(&t, 20);
        let me = all.remove(0);
        let far = all.pop().unwrap();
        assert_eq!(
            route(&me, &t, std::slice::from_ref(&far), &HashSet::new()),
            NextHop::Forward(vec![far])
        );
    }

    #[test]
    fn failing_neighbors_are_ordered_after_healthy_ones_within_each_tier() {
        let t = node("http://target");
        let mut all = sorted_by_distance(&t, 40);
        let me = all.pop().unwrap();
        // all are closer than me; the closest is failing.
        let failing = HashSet::from([canonical_base_url(&all[0].base_url)]);
        let order = forward(next_hop(&me, &t, &all, &HashSet::new(), &failing));
        assert_eq!(order[0], all[1]);
        assert_eq!(order.last().unwrap(), &all[0]);

        // A failing closer neighbor still precedes a healthy not-closer one.
        let all = sorted_by_distance(&t, 40);
        let (closer, me, farther) = (&all[0], &all[1], &all[39]);
        let failing = HashSet::from([canonical_base_url(&closer.base_url)]);
        let ns = [farther.clone(), closer.clone()];
        let order = forward(next_hop(me, &t, &ns, &HashSet::new(), &failing));
        assert_eq!(order, vec![closer.clone(), farther.clone()]);
    }

    #[test]
    fn visited_neighbors_are_skipped() {
        let t = node("http://target");
        let mut all = sorted_by_distance(&t, 40);
        let me = all.pop().unwrap();
        let mut visited = HashSet::new();
        assert_eq!(forward(route(&me, &t, &all, &visited))[0], all[0]);
        visited.insert(canonical_base_url(&all[0].base_url));
        let order = forward(route(&me, &t, &all, &visited));
        assert_eq!(order[0], all[1]);
        assert!(!order.contains(&all[0]));
        for n in &all {
            visited.insert(canonical_base_url(&n.base_url));
        }
        assert_eq!(
            route(&me, &t, &all, &visited),
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
        let order = forward(route(&me, &t, &all, &HashSet::new()));
        assert_eq!(order[0], all[1]);
        assert!(!order.contains(&all[0]));
        let mut ft = t.clone();
        ft.network_id = "other".into();
        assert_eq!(
            route(&me, &ft, &all, &HashSet::new()),
            NextHop::NoRoute(NoRouteReason::ForeignNetwork)
        );
        let mut foreign_target = all[0].clone();
        foreign_target.network_id = "net".into();
        // A same-URL neighbor on another network is not a direct hit.
        assert!(!matches!(
            route(&me, &foreign_target, &all, &HashSet::new()),
            NextHop::Direct { .. }
        ));
    }

    #[test]
    fn deterministic_regardless_of_neighbor_order() {
        let (me, t) = (node("http://me"), node("http://target"));
        let mut ns: Vec<_> = (0..30).map(|i| node(&format!("http://n{i}"))).collect();
        let a = route(&me, &t, &ns, &HashSet::new());
        ns.reverse();
        assert_eq!(a, route(&me, &t, &ns, &HashSet::new()));
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
    fn a_p2p_url_keeps_the_case_of_its_peer_id() {
        assert_eq!(canonical_base_url(" p2p://AbCd// "), "p2p://AbCd");
        assert_ne!(url_key(&node("p2p://AbCd")), url_key(&node("p2p://abcd")));
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
        let d = |n: &OverlayNode| xor(&peer_id_key(n).unwrap(), &tk);
        let my_d = d(&me);
        let order = forward(route(&me, &t, &ns, &HashSet::new()));
        assert_eq!(order.len(), ns.len());
        let closer = order.iter().take_while(|n| d(n) < my_d).count();
        assert_eq!(closer, ns.iter().filter(|n| d(n) < my_d).count());
        for tier in [&order[..closer], &order[closer..]] {
            assert!(tier.windows(2).all(|w| d(&w[0]) <= d(&w[1])));
        }
    }

    #[test]
    fn falls_back_to_url_keys_when_any_peer_id_missing() {
        let me = with_peer_id("http://me");
        let t = with_peer_id("http://target");
        let mut ns: Vec<_> = (0..20)
            .map(|i| with_peer_id(&format!("http://n{i}")))
            .collect();
        ns[0].libp2p_peer_id = None;
        let mixed = route(&me, &t, &ns, &HashSet::new());
        let stripped: Vec<_> = ns.iter().map(strip).collect();
        let expected = route(&strip(&me), &strip(&t), &stripped, &HashSet::new());
        let strip_hop = |h: NextHop| match h {
            NextHop::Forward(v) => NextHop::Forward(v.iter().map(strip).collect()),
            NextHop::Direct { target, fallbacks } => NextHop::Direct {
                target: strip(&target),
                fallbacks: fallbacks.iter().map(strip).collect(),
            },
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

    /// Depth-first routing as the trace forwarder runs it: candidates in order, a failed
    /// branch's nodes stay visited. Asserts no node is entered twice.
    struct Overlay {
        adj: HashMap<usize, HashSet<usize>>,
        nodes: Vec<OverlayNode>,
        dead: HashSet<usize>,
        visited: HashSet<String>,
        entered: Vec<usize>,
    }

    impl Overlay {
        fn new(adj: HashMap<usize, HashSet<usize>>, nodes: Vec<OverlayNode>) -> Self {
            Self {
                adj,
                nodes,
                dead: HashSet::new(),
                visited: HashSet::new(),
                entered: vec![],
            }
        }

        fn route(&mut self, cur: usize, dst: usize, ttl: usize) -> bool {
            assert!(!self.entered.contains(&cur), "entered a node twice");
            self.entered.push(cur);
            self.visited
                .insert(canonical_base_url(&self.nodes[cur].base_url));
            if cur == dst {
                return true;
            }
            if ttl == 0 {
                return false;
            }
            let neighbors: Vec<_> = self.adj[&cur]
                .iter()
                .map(|&i| self.nodes[i].clone())
                .collect();
            let none = HashSet::new();
            let hop = next_hop(
                &self.nodes[cur],
                &self.nodes[dst],
                &neighbors,
                &self.visited,
                &none,
            );
            let cands = match hop {
                NextHop::Direct { target, .. } => vec![target],
                NextHop::Forward(v) => v,
                NextHop::NoRoute(_) => return false,
            };
            for c in cands {
                let idx = self.nodes.iter().position(|x| *x == c).unwrap();
                assert!(self.adj[&cur].contains(&idx));
                if self.dead.contains(&idx)
                    || self.visited.contains(&canonical_base_url(&c.base_url))
                {
                    continue;
                }
                if self.route(idx, dst, ttl - 1) {
                    return true;
                }
            }
            false
        }
    }

    /// On random bounded-degree connected overlays routing never enters a node twice and
    /// reaches every target within a TTL of the node count.
    #[test]
    fn routing_is_loop_free_and_reaches_the_target_in_a_connected_graph() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
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
                let mut o = Overlay::new(adj.clone(), nodes.clone());
                let reached = o.route(src, dst, n);
                assert!(reached, "{src} -> {dst} not reached in a connected graph");
                assert!(o.entered.len() <= n);
            }
        }
    }

    #[test]
    fn a_dead_neighbor_is_routed_around() {
        let mut rng = Rng(0xDEAD_BEEF_1234_5678);
        let n = 30;
        let nodes: Vec<_> = (0..n)
            .map(|i| node(&format!("http://r{i}.example")))
            .collect();
        // A ring plus the dead node's own bypass edges keeps the rest connected.
        let mut adj: HashMap<usize, HashSet<usize>> = (0..n).map(|i| (i, HashSet::new())).collect();
        for i in 0..n {
            for d in [1, 2] {
                let j = (i + d) % n;
                adj.get_mut(&i).unwrap().insert(j);
                adj.get_mut(&j).unwrap().insert(i);
            }
        }
        for _ in 0..50 {
            let (src, dst, dead) = (rng.below(n), rng.below(n), rng.below(n));
            if dead == src || dead == dst {
                continue;
            }
            let mut o = Overlay::new(adj.clone(), nodes.clone());
            o.dead.insert(dead);
            assert!(o.route(src, dst, n));
        }
    }
}
