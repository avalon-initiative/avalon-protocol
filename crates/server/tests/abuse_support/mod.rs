//! Shared fixtures for the relay and hole-punch abuse tests: in-process loopback swarms and a
//! raw stream peer that writes arbitrary bytes on any protocol.
#![allow(dead_code)]

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::{AutonatSettings, ReachabilitySnapshot};
use avalon_server::relay::{RelayClientSettings, RelayServerSettings, RelaySettings};
use libp2p::futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, StreamExt};
use libp2p::request_response::{self, OutboundFailure, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    identify, identity, noise, relay, tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm,
    SwarmBuilder,
};

pub const WAIT: Duration = Duration::from_secs(30);
pub const NETWORK: &str = "avalon-test";

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn ipv6_available() -> bool {
    std::net::TcpListener::bind("[::1]:0").is_ok()
}

pub fn autonat() -> AutonatSettings {
    AutonatSettings {
        allow_private: true,
        boot_delay: Duration::from_millis(100),
        retry_interval: Duration::from_millis(300),
        throttle_server_period: Duration::ZERO,
        ..AutonatSettings::default()
    }
}

pub fn config(listen: &str, external: Option<&str>, relay: RelaySettings) -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: listen.parse().unwrap(),
        external_addr: external.map(|e| e.parse().unwrap()),
        autonat: autonat(),
        relay,
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

/// A dialable relay serving with `limits`, and its direct address ending in `/p2p/<id>`.
pub async fn relay_node(limits: RelayServerSettings) -> (DhtHandle, Multiaddr) {
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let relay = RelaySettings {
        server: Some(limits),
        ..RelaySettings::default()
    };
    let node = dht::start(PeerTable::new(), config(&addr, Some(&addr), relay)).await;
    let full = format!("{addr}/p2p/{}", node.peer_id).parse().unwrap();
    (node, full)
}

pub fn client_settings(relays: Vec<Multiaddr>, max: usize) -> RelayClientSettings {
    RelayClientSettings {
        max_reservations: max,
        relay_addrs: relays,
        allow_private: true,
        retry_backoff: Duration::from_secs(60),
        pending_timeout: Duration::from_secs(10),
        reconcile_interval: Duration::from_millis(200),
        ..RelayClientSettings::default()
    }
}

pub fn peer_info(node: &DhtHandle, addrs: Vec<String>) -> PeerInfo {
    PeerInfo {
        identity_bound: true,
        base_url: format!("http://{}.test", node.peer_id),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
        libp2p_listen_addrs: addrs,
        connectivity: None,
        witness: None,
    }
}

pub fn introduce(into: &PeerTable, node: &DhtHandle) {
    into.upsert(peer_info(
        node,
        node.listen_addrs.iter().map(|a| a.to_string()).collect(),
    ));
}

/// A `private` node (IPv6 loopback only) that AutoNAT probes through `probe_via` and that
/// reserves on `relays`.
pub async fn private_node(relays: Vec<Multiaddr>, max: usize, probe_via: &DhtHandle) -> DhtHandle {
    let peers = PeerTable::new();
    let relay = RelaySettings {
        client: client_settings(relays, max),
        ..RelaySettings::default()
    };
    let node = dht::start(peers.clone(), config("/ip6/::1/tcp/0", None, relay)).await;
    introduce(&peers, probe_via);
    node
}

pub async fn wait_snapshot<T>(
    node: &DhtHandle,
    what: &str,
    mut pick: impl FnMut(&ReachabilitySnapshot) -> Option<T>,
) -> T {
    let mut rx = node.reachability.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(found) = pick(&rx.borrow_and_update()) {
                return found;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The relayed address of the first reservation `node` holds on `relay`.
pub async fn reserved_on(node: &DhtHandle, relay: PeerId) -> Multiaddr {
    wait_snapshot(node, "a reservation", |s| {
        s.relay_reservations
            .iter()
            .find(|r| r.relay_peer_id == relay.to_string())
            .map(|r| r.relayed_addr.parse().unwrap())
    })
    .await
}

pub async fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[derive(NetworkBehaviour)]
pub struct Raw {
    pub relay_client: relay::client::Behaviour,
    pub identify: identify::Behaviour,
}

/// A node with no listener that can only reach others through relays; a fresh keypair each call.
pub fn raw_swarm() -> Swarm<Raw> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|key, relay_client| Raw {
            relay_client,
            identify: identify::Behaviour::new(identify::Config::new(
                format!("/avalon/dht/1.0.0/{NETWORK}"),
                key.public(),
            )),
        })
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build()
}

/// Dials `addr` and waits until `target` is connected over a relayed connection and has
/// identified itself, or the dial fails.
pub async fn dial_through_relay(
    swarm: &mut Swarm<Raw>,
    addr: Multiaddr,
    target: PeerId,
) -> Result<(), String> {
    swarm.dial(addr).map_err(|e| e.to_string())?;
    let (mut relayed, mut identified) = (false, false);
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::ConnectionEstablished {
                    peer_id, endpoint, ..
                } if peer_id == target => relayed = endpoint.is_relayed(),
                SwarmEvent::Behaviour(RawEvent::Identify(identify::Event::Received {
                    peer_id,
                    ..
                })) if peer_id == target => identified = true,
                SwarmEvent::OutgoingConnectionError { error, .. } => return Err(error.to_string()),
                _ => {}
            }
            if relayed && identified {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| "timed out".to_string())?
}

/// Reserves a slot on `relay` with a bare client and reports whether it was accepted.
pub async fn raw_reserve(swarm: &mut Swarm<Raw>, relay: &Multiaddr) -> bool {
    swarm
        .listen_on(relay.clone().with(libp2p::multiaddr::Protocol::P2pCircuit))
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(RawEvent::RelayClient(
                    relay::client::Event::ReservationReqAccepted { .. },
                )) => return true,
                SwarmEvent::ListenerClosed { .. } => return false,
                _ => {}
            }
        }
    })
    .await
    .expect("reservation neither accepted nor refused")
}

/// What a hostile stream peer writes: `prefix`, then, while the receiver keeps reading,
/// `flood_for` of further filler bytes. A receiver that refuses the frame resets the stream and
/// ends the flood early.
#[derive(Debug, Clone)]
pub struct Hostile {
    pub prefix: Vec<u8>,
    pub flood_for: Duration,
}

impl Hostile {
    pub fn bytes(prefix: &[u8]) -> Self {
        Self {
            prefix: prefix.to_vec(),
            flood_for: Duration::ZERO,
        }
    }

    pub fn flooding(prefix: &[u8], flood_for: Duration) -> Self {
        Self {
            prefix: prefix.to_vec(),
            flood_for,
        }
    }
}

/// Bytes the hostile peer got onto the wire across all its requests.
pub type Written = Arc<AtomicUsize>;

#[derive(Clone)]
pub struct RawCodec {
    pub written: Written,
}

const FLOOD_CHUNK: usize = 16 * 1024;
const FLOOD_PACE: Duration = Duration::from_millis(5);

impl request_response::Codec for RawCodec {
    type Protocol = StreamProtocol;
    type Request = Hostile;
    type Response = Vec<u8>;

    /// One read: a peer that keeps its side open never reaches EOF.
    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Hostile>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = vec![0u8; 8192];
        let n = io.read(&mut buf).await?;
        buf.truncate(n);
        Ok(Hostile::bytes(&buf))
    }

    async fn read_response<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Vec<u8>>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut out = Vec::new();
        (&mut *io).take(1 << 20).read_to_end(&mut out).await?;
        Ok(out)
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Hostile,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        io.write_all(&req.prefix).await?;
        self.written.fetch_add(req.prefix.len(), Ordering::SeqCst);
        io.flush().await?;
        let end = tokio::time::Instant::now() + req.flood_for;
        let filler = vec![0xA5u8; FLOOD_CHUNK];
        while tokio::time::Instant::now() < end {
            io.write_all(&filler).await?;
            io.flush().await?;
            self.written.fetch_add(FLOOD_CHUNK, Ordering::SeqCst);
            tokio::time::sleep(FLOOD_PACE).await;
        }
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Vec<u8>,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        io.write_all(&res).await?;
        io.flush().await
    }
}

#[derive(NetworkBehaviour)]
pub struct Hostiles {
    pub relay_client: relay::client::Behaviour,
    pub identify: identify::Behaviour,
    pub raw: request_response::Behaviour<RawCodec>,
}

/// A peer that speaks `protocol` as raw bytes, with its own fresh keypair.
pub fn hostile_swarm(protocol: StreamProtocol) -> (Swarm<Hostiles>, Written) {
    let written = Written::default();
    let codec = RawCodec {
        written: written.clone(),
    };
    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|key, relay_client| Hostiles {
            relay_client,
            identify: identify::Behaviour::new(identify::Config::new(
                format!("/avalon/dht/1.0.0/{NETWORK}"),
                key.public(),
            )),
            raw: request_response::Behaviour::with_codec(
                codec,
                [(protocol, ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(Duration::from_secs(4)),
            ),
        })
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();
    (swarm, written)
}

/// How one hostile request ended.
#[derive(Debug)]
pub enum Outcome {
    /// The peer answered (possibly with nothing) and closed.
    Answered(Vec<u8>),
    /// The stream failed, was refused or was reset.
    Failed(OutboundFailure),
}

/// Connects `swarm` to `target` and waits until the connection is up.
pub async fn connect(swarm: &mut Swarm<Hostiles>, target: Multiaddr, peer: PeerId) {
    swarm.dial(target).unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let SwarmEvent::ConnectionEstablished { peer_id, .. } =
                swarm.select_next_some().await
            {
                if peer_id == peer {
                    return;
                }
            }
        }
    })
    .await
    .expect("hostile peer connects");
}

/// Sends every request in `requests` to `peer` at once and returns how each one ended.
pub async fn send_all(
    swarm: &mut Swarm<Hostiles>,
    peer: PeerId,
    requests: Vec<Hostile>,
) -> Vec<Outcome> {
    let total = requests.len();
    let mut ids = std::collections::HashSet::new();
    for r in requests {
        ids.insert(swarm.behaviour_mut().raw.send_request(&peer, r));
    }
    let mut outcomes = Vec::new();
    tokio::time::timeout(WAIT, async {
        while outcomes.len() < total {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(HostilesEvent::Raw(request_response::Event::Message {
                    message: request_response::Message::Response { response, .. },
                    ..
                })) => outcomes.push(Outcome::Answered(response)),
                SwarmEvent::Behaviour(HostilesEvent::Raw(
                    request_response::Event::OutboundFailure { error, .. },
                )) => outcomes.push(Outcome::Failed(error)),
                _ => {}
            }
        }
    })
    .await
    .expect("every hostile request ends");
    outcomes
}

/// Whether a connection of `swarm` closes within `dur`.
pub async fn closes_within(swarm: &mut Swarm<Raw>, dur: Duration) -> bool {
    tokio::time::timeout(dur, async {
        loop {
            if let SwarmEvent::ConnectionClosed { .. } = swarm.select_next_some().await {
                return;
            }
        }
    })
    .await
    .is_ok()
}

/// Runs `swarm` for up to `wait`, answering each inbound request with the next of `replies`
/// (the last one repeats); returns how many it answered and what they carried.
pub async fn answer_inbound(
    swarm: &mut Swarm<Hostiles>,
    replies: &[Vec<u8>],
    wait: Duration,
) -> Vec<Vec<u8>> {
    let mut seen = Vec::new();
    let _ = tokio::time::timeout(wait, async {
        loop {
            if let SwarmEvent::Behaviour(HostilesEvent::Raw(request_response::Event::Message {
                message:
                    request_response::Message::Request {
                        request, channel, ..
                    },
                ..
            })) = swarm.select_next_some().await
            {
                let reply = replies[seen.len().min(replies.len() - 1)].clone();
                seen.push(request.prefix);
                let _ = swarm.behaviour_mut().raw.send_response(channel, reply);
                return;
            }
        }
    })
    .await;
    seen
}
