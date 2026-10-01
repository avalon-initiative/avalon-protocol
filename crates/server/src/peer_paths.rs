//! The kind of libp2p connection this node holds to each peer, kept by the DHT worker so probe
//! and trace can label what they measured. Connectivity only; never trust or authority.
//!
//! A peer with several open connections reports the worst one, so a measurement that may have
//! used a relay is never labeled direct.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use avalon_protocol::connectivity::PathType;
use libp2p::swarm::ConnectionId;
use libp2p::PeerId;

#[derive(Default)]
struct Inner {
    /// Open connections: the peer and whether the connection runs through a relay.
    conns: HashMap<ConnectionId, (PeerId, bool)>,
    /// Direct connections that came from a successful hole punch.
    punched: HashSet<ConnectionId>,
}

/// Shared between the worker, which writes it, and request handlers, which read it.
#[derive(Clone, Default)]
pub struct PeerPaths {
    inner: Arc<RwLock<Inner>>,
}

impl PeerPaths {
    pub(crate) fn connection_opened(&self, peer: PeerId, conn: ConnectionId, relayed: bool) {
        self.write().conns.insert(conn, (peer, relayed));
    }

    pub(crate) fn hole_punched(&self, conn: ConnectionId) {
        self.write().punched.insert(conn);
    }

    pub(crate) fn connection_closed(&self, conn: ConnectionId) {
        let mut inner = self.write();
        inner.conns.remove(&conn);
        inner.punched.remove(&conn);
    }

    /// The worst path among `peer`'s open connections; `None` when none is open.
    pub fn path(&self, peer: &PeerId) -> Option<PathType> {
        let inner = self.inner.read().expect("peer paths lock poisoned");
        inner
            .conns
            .iter()
            .filter(|(_, (p, _))| p == peer)
            .map(|(id, (_, relayed))| {
                if *relayed {
                    PathType::Relayed
                } else if inner.punched.contains(id) {
                    PathType::Traversed
                } else {
                    PathType::Direct
                }
            })
            .reduce(PathType::worse)
    }

    /// How many connections are open to `peer`.
    pub fn connections(&self, peer: &PeerId) -> usize {
        let inner = self.inner.read().expect("peer paths lock poisoned");
        inner.conns.values().filter(|(p, _)| p == peer).count()
    }

    /// The path a request took: an HTTP response came over a direct dial of the peer's URL,
    /// a stream response over whatever connection the peer has open.
    pub fn path_of(&self, via_stream: bool, peer: Option<&PeerId>) -> Option<PathType> {
        if !via_stream {
            return Some(PathType::Direct);
        }
        peer.and_then(|p| self.path(p))
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().expect("peer paths lock poisoned")
    }
}

/// The path an announce round trip to the neighbor `base_url` took. A stream with no open
/// connection left is labeled relayed rather than claiming a better path than is known.
pub fn round_trip_path(peers: &crate::nodes::PeerTable, base_url: &str) -> PathType {
    match crate::node_http::parse_p2p_base(&peers.transport_url(base_url)) {
        Some(id) => peers.paths().path(&id).unwrap_or(PathType::Relayed),
        None => PathType::Direct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(n: usize) -> ConnectionId {
        ConnectionId::new_unchecked(n)
    }

    #[test]
    fn classifies_direct_traversed_and_relayed() {
        let paths = PeerPaths::default();
        let (d, t, r, none) = (
            PeerId::random(),
            PeerId::random(),
            PeerId::random(),
            PeerId::random(),
        );
        paths.connection_opened(d, conn(1), false);
        paths.connection_opened(t, conn(2), false);
        paths.hole_punched(conn(2));
        paths.connection_opened(r, conn(3), true);
        assert_eq!(paths.path(&d), Some(PathType::Direct));
        assert_eq!(paths.path(&t), Some(PathType::Traversed));
        assert_eq!(paths.path(&r), Some(PathType::Relayed));
        assert_eq!(paths.path(&none), None);
    }

    #[test]
    fn a_punch_reported_before_the_connection_still_classifies() {
        let paths = PeerPaths::default();
        let p = PeerId::random();
        paths.hole_punched(conn(5));
        paths.connection_opened(p, conn(5), false);
        assert_eq!(paths.path(&p), Some(PathType::Traversed));
    }

    #[test]
    fn a_surviving_relayed_connection_keeps_the_peer_labeled_relayed() {
        let paths = PeerPaths::default();
        let p = PeerId::random();
        paths.connection_opened(p, conn(1), true);
        paths.connection_opened(p, conn(2), false);
        paths.hole_punched(conn(2));
        assert_eq!(paths.path(&p), Some(PathType::Relayed));
        paths.connection_closed(conn(1));
        assert_eq!(paths.path(&p), Some(PathType::Traversed));
        paths.connection_closed(conn(2));
        assert_eq!(paths.path(&p), None);
    }

    #[test]
    fn http_is_direct_and_a_stream_uses_the_connection() {
        let paths = PeerPaths::default();
        let p = PeerId::random();
        assert_eq!(paths.path_of(false, None), Some(PathType::Direct));
        assert_eq!(paths.path_of(true, Some(&p)), None);
        assert_eq!(paths.path_of(true, None), None);
        paths.connection_opened(p, conn(1), true);
        assert_eq!(paths.path_of(true, Some(&p)), Some(PathType::Relayed));
        assert_eq!(paths.path_of(false, Some(&p)), Some(PathType::Direct));
    }

    fn table_with(base_url: &str, id: &PeerId, relayed: bool) -> crate::nodes::PeerTable {
        let peers = crate::nodes::PeerTable::new();
        peers.upsert(crate::nodes::PeerInfo {
            identity_bound: true,
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: "n".to_string(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: Some(id.to_string()),
            libp2p_listen_addrs: vec![],
            connectivity: relayed.then_some(avalon_protocol::connectivity::Connectivity::Relayed),
            witness: None,
        });
        peers
    }

    #[test]
    fn announce_round_trips_are_labeled_by_transport() {
        let id = PeerId::random();
        let direct = table_with("http://direct.test", &id, false);
        assert_eq!(
            round_trip_path(&direct, "http://direct.test"),
            PathType::Direct
        );

        let relayed = table_with("http://relayed.test", &id, true);
        assert_eq!(
            round_trip_path(&relayed, "http://relayed.test"),
            PathType::Relayed,
            "no open connection: never claim better than known"
        );
        relayed.paths().connection_opened(id, conn(1), false);
        assert_eq!(
            round_trip_path(&relayed, "http://relayed.test"),
            PathType::Direct
        );
        relayed.paths().hole_punched(conn(1));
        assert_eq!(
            round_trip_path(&relayed, "http://relayed.test"),
            PathType::Traversed
        );
    }
}
