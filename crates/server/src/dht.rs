//! Interest-scoped realtime routing via a libp2p Kademlia DHT. This module owns two
//! things: this node's libp2p identity/bootstrap, and a small
//! command channel exposing the swarm's `put_record`/`get_record` to the
//! rest of the process — the swarm itself still lives entirely
//! inside [`run_worker`]'s spawned task, so any other code that wants to
//! touch the DHT does it by sending a [`DhtCommand`] rather than reaching
//! into the swarm directly. `crate::interest` is the one real caller today,
//! for guild-channel/conversation interest registration and lookup; no
//! actual relay re-scoping happens here.
//!
//! **A libp2p `PeerId` is a brand-new identity domain, not a reuse of any
//! existing key.** This codebase already has three separate key domains
//! (player keys, issuer keys, the settlement log operator's key
//! — see `avalon_protocol::sth`) and none of them fit: player/issuer keys
//! are about attestation/authorship, never held by a server process at all,
//! and the settlement key's lifecycle (rotatable, tied to STH-signing) is
//! semantically unrelated to peer-transport identity. A node now holds a
//! *fourth*, independent identity purely for DHT transport — see
//! [`load_or_generate_identity_from_env`].
//!
//! **Bootstrap reuses the existing peer-announce mechanism, it doesn't
//! replace it.** `PeerInfo` (extended
//! with `libp2p_peer_id`/`libp2p_listen_addrs`) is gossiped
//! through the exact same `POST /nodes/announce` mechanism `crate::nodes`
//! already runs — a peer's DHT identity just rides along with everything
//! else it already announces. [`run_worker`] here does no announcing of its
//! own; it only watches `PeerTable` (already kept fresh by
//! `nodes::run_worker`) for peers whose DHT identity it hasn't dialed yet.
//!
//! **AutoNAT** (client and server) shares this swarm; see `crate::reachability`. Detection runs in
//! the worker, never blocks [`start`], and publishes into [`DhtHandle::reachability`].
//!
//! **Circuit relay v2** (client and opt-in server) also shares this swarm; see `crate::relay`. A
//! node AutoNAT finds `private` reserves slots on relays and lists them in
//! [`DhtHandle::reachability`].
//!
//! **`AVALON_DHT_ENABLED` defaults to on** — an opt-*out*
//! escape hatch, not an opt-in gate. There are no real deployments of
//! this software outside this project's own development sandbox yet, so
//! there was no one to protect with an opt-in default, and DHT-based
//! interest routing only becomes useful once nodes actually participate
//! in it from the start rather than each operator individually deciding
//! to turn it on later. Set it to `false` for a single-node/private
//! self-hoster who wants zero DHT overhead, or if a real problem surfaces
//! while this is still genuinely young, unproven-at-real-scale code.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libp2p::connection_limits;
use libp2p::futures::StreamExt;
use libp2p::kad::{self, store::MemoryStore, Mode, QueryId};
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId, ProtocolSupport, ResponseChannel};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::{ConnectionId, DialError, NetworkBehaviour, SwarmEvent};
use libp2p::{
    autonat, dcutr, identify, identity, noise, relay, tcp, yamux, Multiaddr, PeerId,
    StreamProtocol, Swarm, SwarmBuilder,
};
use tokio::sync::{mpsc, oneshot};

use crate::node_http::{
    InboundPermit, InboundService, NodeHttpCodec, NodeHttpError, NodeHttpRequest, NodeHttpResponse,
    NodeHttpSettings, RouterSlot, StreamErrorKind, StreamHandle,
};
use crate::nodes::{PeerInfo, PeerTable};
use crate::outbound_policy::OutboundPolicy;
use crate::reachability::{
    peer_ip_allowed, AutonatSettings, HolePunchOutcome, Reachability, ReachabilityHandle,
};
use crate::relay::{RelayClient, RelayServerStats, RelaySettings};

/// Bounded so a burst of interest registrations/lookups can't grow this
/// unboundedly if [`run_worker`] is momentarily busy — same rationale
/// `chat::UPDATE_CHANNEL_CAPACITY` already documents for its own bounded
/// channel. A full channel backs the sender's `.send().await` up rather
/// than dropping silently, which is the right tradeoff here: a missed
/// interest registration is a real, if temporary, routing gap, not a
/// disposable UI tick.
const COMMAND_CHANNEL_CAPACITY: usize = 256;

/// A request from elsewhere in this process to the DHT swarm — the only
/// way anything outside [`run_worker`] touches `kad`, since the swarm
/// itself never leaves that task. See [`DhtHandle::commands`].
pub enum DhtCommand {
    /// Stores `value` under `key`, expiring after `ttl` — the caller
    /// (`crate::interest`) is expected to re-send this periodically for as
    /// long as the registration should stay live; there is no separate
    /// "deregister" command; letting a record's TTL lapse is the only way
    /// a registration goes away — no explicit cleanup needed on every
    /// disconnect path.
    PutRecord {
        key: Vec<u8>,
        value: Vec<u8>,
        ttl: Duration,
    },
    /// Looks up every value currently stored under `key`, however many
    /// distinct publishers that turns out to be, and reports them once the
    /// query completes (bounded by kad's own internal query timeout —
    /// never hangs forever).
    GetRecord {
        key: Vec<u8>,
        respond_to: oneshot::Sender<Vec<Vec<u8>>>,
    },
    /// One node-to-node HTTP exchange with `peer` over a stream, dialing it first if needed.
    HttpRequest {
        peer: PeerId,
        request: NodeHttpRequest,
        respond_to: oneshot::Sender<Result<NodeHttpResponse, NodeHttpError>>,
    },
}

pub type DhtCommandSender = mpsc::Sender<DhtCommand>;

/// One in-flight `get_record` query's response channel plus whatever
/// values it's accumulated so far — see [`run_worker`]'s `pending_gets`.
type PendingGet = (oneshot::Sender<Vec<Vec<u8>>>, Vec<Vec<u8>>);

/// Base namespace for the informational `identify` exchange — see
/// [`identify_protocol_version`], which appends this node's `network_id`.
const IDENTIFY_PROTOCOL_VERSION: &str = "/avalon/dht/1.0.0";

/// Base namespace for this swarm's actual Kademlia wire protocol — see
/// [`kad_protocol_name`], which appends this node's `network_id`. Unlike
/// `IDENTIFY_PROTOCOL_VERSION`, this one is load-bearing: libp2p's
/// multistream-select negotiates substreams by exact protocol-id string
/// match, so two swarms configured with different `kad_protocol_name`
/// values genuinely cannot exchange a single Kademlia RPC with each other —
/// not "the message gets ignored," the substream negotiation itself fails.
const KAD_PROTOCOL_VERSION: &str = "/avalon/kad/1.0.0";

/// Every network this node's DHT swarm exists for gets its own
/// namespaced Kademlia protocol id, so a peer configured for a different
/// `network_id` can never negotiate a Kademlia substream with this node at
/// all — the real fix for a gap previously left as a known,
/// accepted limitation ("the DHT keyspace itself has no `network_id`
/// segregation... in practice this doesn't leak across networks today only
/// because bootstrap can itself only ever reach peers already
/// admitted into this same network's peer table, not because the DHT layer
/// enforces it directly"). This closes that gap structurally: reachability
/// at the DHT layer is now bounded to `network_id`, not merely incidental
/// on `nodes::announce`'s own HTTP-layer rejection of a mismatched
/// `network_id` upstream of it. Two nodes with the same `network_id` still
/// interoperate exactly as before — this changes nothing about behavior
/// within one network, only what's reachable across two different ones.
fn kad_protocol_name(network_id: &str) -> StreamProtocol {
    StreamProtocol::try_from_owned(format!("{KAD_PROTOCOL_VERSION}/{network_id}"))
        .expect("a network_id-scoped protocol string always starts with '/'")
}

/// Extra time past the handler timeout for the answer to cross the stream.
const NODE_HTTP_ANSWER_GRACE: Duration = Duration::from_secs(5);

/// The node-to-node HTTP stream protocol, namespaced by network id like [`kad_protocol_name`].
fn node_http_protocol(network_id: &str) -> StreamProtocol {
    crate::node_http::protocol_name(network_id)
}

/// The `identify` protocol-version string this node advertises — informs
/// [`run_worker`]'s own defense-in-depth check (disconnecting a peer whose
/// advertised value doesn't match, rather than relying solely on
/// [`kad_protocol_name`] silently failing to negotiate). Purely an
/// exchanged application-level field, not a wire protocol id the way
/// [`kad_protocol_name`] is — `identify`'s own substream protocol id is
/// fixed by the `identify` crate itself and isn't network-scoped by this.
fn identify_protocol_version(network_id: &str) -> String {
    format!("{IDENTIFY_PROTOCOL_VERSION}/{network_id}")
}

/// How often [`run_worker`] re-scans `PeerTable` for DHT identities it
/// hasn't dialed yet, when `AVALON_DHT_BOOTSTRAP_SCAN_INTERVAL_SECS` is
/// unset. `PeerTable` itself already refreshes on
/// `AVALON_ANNOUNCE_INTERVAL_SECS` (default 180s); scanning noticeably
/// faster than that just means a newly-announced peer's DHT identity is
/// picked up sooner without needing its own separate signal. Configurable
/// so a live test can turn this down without waiting out a
/// real deployment's cadence — see
/// `crates/server/tests/interest_dht.rs`.
const DEFAULT_BOOTSTRAP_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// How long [`start`] waits, at most, to observe this node's own listen
/// addresses before returning — binding `0.0.0.0` normally yields one
/// `NewListenAddr` event per local interface within milliseconds, so this
/// is a generous ceiling, not an expected wait.
const INITIAL_LISTEN_COLLECTION_WINDOW: Duration = Duration::from_secs(2);

#[derive(NetworkBehaviour)]
struct DhtBehaviour {
    kad: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    autonat: Toggle<autonat::Behaviour>,
    relay_client: relay::client::Behaviour,
    relay_server: Toggle<relay::Behaviour>,
    dcutr: Toggle<dcutr::Behaviour>,
    limits: connection_limits::Behaviour,
    node_http: request_response::Behaviour<NodeHttpCodec>,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityLoadError {
    #[error("AVALON_LIBP2P_IDENTITY_KEY is not valid hex: {0}")]
    InvalidHex(hex::FromHexError),
    #[error("AVALON_LIBP2P_IDENTITY_KEY must decode to exactly 32 bytes, got {0}")]
    WrongLength(usize),
}

/// Loads this node's libp2p identity keypair from
/// `AVALON_LIBP2P_IDENTITY_KEY` (a raw 32-byte Ed25519 seed, hex-encoded —
/// the same convention `avalon_protocol::sth`'s `AVALON_SETTLEMENT_SIGNING_KEY`
/// already establishes), or generates a fresh one when unset.
///
/// Unlike the settlement key, an unset value is **not** an error: this is a
/// brand-new identity domain with no existing deployment depending on it,
/// so an ephemeral per-restart identity is a reasonable dev default — the
/// worst case is other nodes needing to re-learn this node's `PeerId` after
/// a restart, a reversible availability blip, never a security gate (the
/// same posture the protocol-version floor already takes for peer
/// admission). An operator who wants a stable `PeerId` sets the env var.
pub fn load_or_generate_identity_from_env() -> Result<identity::Keypair, IdentityLoadError> {
    match std::env::var("AVALON_LIBP2P_IDENTITY_KEY") {
        Ok(hex_value) => {
            let bytes = hex::decode(&hex_value).map_err(IdentityLoadError::InvalidHex)?;
            let len = bytes.len();
            let mut seed: [u8; 32] = bytes
                .try_into()
                .map_err(|_| IdentityLoadError::WrongLength(len))?;
            Ok(identity::Keypair::ed25519_from_bytes(&mut seed)
                .expect("a 32-byte seed is always a valid Ed25519 key"))
        }
        Err(_) => {
            tracing::warn!(
                "avalon-dht: AVALON_LIBP2P_IDENTITY_KEY unset — generating an ephemeral libp2p \
                 identity for this run; this node's PeerId will change on every restart. Set \
                 AVALON_LIBP2P_IDENTITY_KEY for a stable identity peers don't need to relearn."
            );
            Ok(identity::Keypair::generate_ed25519())
        }
    }
}

/// `AVALON_DHT_ENABLED`/`AVALON_LIBP2P_LISTEN_ADDR`/
/// `AVALON_LIBP2P_EXTERNAL_ADDR` resolved once at startup. `None` (via
/// [`from_env`](Self::from_env)) means the DHT work doesn't run at
/// all — every deployment without it configured behaves unchanged.
pub struct DhtConfig {
    pub identity: identity::Keypair,
    /// This node's own `chain.network_id()`, threaded through to
    /// [`build_swarm`] so the Kademlia protocol id — and the `identify`
    /// exchange's advertised version — are both scoped to it. Every
    /// `DhtConfig` is built from a real, already-validated `network_id`
    /// (`AVALON_NETWORK_ID` is required at startup, never defaulted — see
    /// `main.rs`), so this is never empty in practice.
    pub network_id: String,
    pub listen_addr: Multiaddr,
    /// Operator-stated address peers should dial (a containerized node only sees its
    /// internal addresses). It is advertised as given and also probed by AutoNAT.
    pub external_addr: Option<Multiaddr>,
    /// AutoNAT reachability detection and dial-back settings.
    pub autonat: AutonatSettings,
    /// Circuit relay client and server settings.
    pub relay: RelaySettings,
    /// `AVALON_DHT_BOOTSTRAP_SCAN_INTERVAL_SECS`, defaulting to
    /// [`DEFAULT_BOOTSTRAP_SCAN_INTERVAL`] — see that constant's own doc
    /// comment.
    pub bootstrap_scan_interval: Duration,
}

impl DhtConfig {
    /// `Ok(None)` when `AVALON_DHT_ENABLED` is explicitly set to a falsy
    /// value (`false`/`0`) — unset defaults to *enabled* (an
    /// opt-out escape hatch, not an opt-in gate — see this module's own
    /// doc comment for why). `AVALON_LIBP2P_LISTEN_ADDR` defaults to
    /// `/ip4/0.0.0.0/tcp/0` (an ephemeral port on every interface),
    /// matching this repo's existing "sane default, explicit override"
    /// convention (e.g. `AVALON_SERVER_ADDR`). `AVALON_LIBP2P_EXTERNAL_ADDR`
    /// is unset by default (native, non-containerized deployments don't
    /// need it — see `external_addr`'s own doc comment).
    pub fn from_env(network_id: &str) -> Result<Option<Self>, String> {
        let enabled = std::env::var("AVALON_DHT_ENABLED")
            .map(|v| !(v.eq_ignore_ascii_case("false") || v == "0"))
            .unwrap_or(true);
        if !enabled {
            return Ok(None);
        }

        let identity = load_or_generate_identity_from_env().map_err(|e| e.to_string())?;
        let listen_addr = std::env::var("AVALON_LIBP2P_LISTEN_ADDR")
            .unwrap_or_else(|_| "/ip4/0.0.0.0/tcp/0".to_string())
            .parse::<Multiaddr>()
            .map_err(|e| format!("AVALON_LIBP2P_LISTEN_ADDR is not a valid multiaddr: {e}"))?;
        let external_addr = std::env::var("AVALON_LIBP2P_EXTERNAL_ADDR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<Multiaddr>().map_err(|e| {
                    format!("AVALON_LIBP2P_EXTERNAL_ADDR is not a valid multiaddr: {e}")
                })
            })
            .transpose()?;
        let bootstrap_scan_interval = std::env::var("AVALON_DHT_BOOTSTRAP_SCAN_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_BOOTSTRAP_SCAN_INTERVAL);
        let policy = OutboundPolicy::from_env();
        let autonat = AutonatSettings::from_env(policy)?;
        let relay = RelaySettings::from_env(policy)?;

        Ok(Some(Self {
            identity,
            network_id: network_id.to_string(),
            listen_addr,
            external_addr,
            autonat,
            relay,
            bootstrap_scan_interval,
        }))
    }
}

/// This node's own DHT identity, as observed once at startup — handed to
/// `crate::nodes::run_worker` so it rides along on every outbound announce
/// (see this module's own doc comment), plus the [`DhtCommandSender`]
/// any other code uses to issue `put_record`/`get_record` against
/// the swarm this handle was created from.
pub struct DhtHandle {
    pub peer_id: PeerId,
    /// Addresses the swarm bound locally. Not advertised: announce uses
    /// [`ReachabilityHandle::advertised_addrs`].
    pub listen_addrs: Vec<Multiaddr>,
    pub commands: DhtCommandSender,
    pub reachability: ReachabilityHandle,
    /// Counters for the relay server role; all zero unless it is enabled.
    pub relay_stats: Arc<RelayServerStats>,
    /// Fill this once the axum router exists; until then stream requests get a 503.
    pub router_slot: RouterSlot,
    /// The client side of the node-to-node stream transport.
    pub node_http: StreamHandle,
}

/// How long a connection with no active substream stays open. Long enough that AutoNAT probes
/// and relay reservations find a connected server between probe intervals.
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(120);

fn build_swarm(
    identity: identity::Keypair,
    network_id: &str,
    autonat_settings: &AutonatSettings,
    relay_settings: &RelaySettings,
    node_http: &NodeHttpSettings,
) -> Swarm<DhtBehaviour> {
    let peer_id = PeerId::from(identity.public());
    let kad_config = kad::Config::new(kad_protocol_name(network_id));
    SwarmBuilder::with_existing_identity(identity)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("TCP/noise/yamux transport construction is infallible for these fixed configs")
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .expect("relay client transport construction is infallible for these fixed configs")
        .with_behaviour(|key, relay_client| DhtBehaviour {
            kad: kad::Behaviour::with_config(peer_id, MemoryStore::new(peer_id), kad_config),
            identify: identify::Behaviour::new(identify::Config::new(
                identify_protocol_version(network_id),
                key.public(),
            )),
            autonat: autonat_settings
                .enabled
                .then(|| autonat::Behaviour::new(peer_id, autonat_settings.libp2p_config()))
                .into(),
            relay_client,
            relay_server: relay_settings
                .server
                .as_ref()
                .map(|s| relay::Behaviour::new(peer_id, s.libp2p_config()))
                .into(),
            dcutr: relay_settings
                .hole_punching
                .then(|| dcutr::Behaviour::new(peer_id))
                .into(),
            node_http: request_response::Behaviour::with_codec(
                NodeHttpCodec::new(node_http),
                [(node_http_protocol(network_id), ProtocolSupport::Full)],
                // Outlasts the handler's own bound so its 504 can still be sent.
                request_response::Config::default()
                    .with_request_timeout(node_http.timeout + NODE_HTTP_ANSWER_GRACE)
                    .with_max_concurrent_streams(crate::node_http::MAX_CONCURRENT_STREAMS),
            ),
            limits: connection_limits::Behaviour::new(
                connection_limits::ConnectionLimits::default()
                    .with_max_established(Some(node_http.max_connections))
                    .with_max_established_per_peer(Some(node_http.max_connections_per_peer))
                    .with_max_pending_incoming(Some(node_http.max_pending_incoming)),
            ),
        })
        .expect("behaviour construction from a fixed, valid config is infallible")
        .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_CONNECTION_TIMEOUT))
        .build()
}

/// Builds the DHT swarm, binds `config.listen_addr`, and spawns the
/// long-running worker that keeps it fed from `peers` (the peer table).
/// Returns as soon as this node's own listen addresses are known (bounded
/// by [`INITIAL_LISTEN_COLLECTION_WINDOW`]) so the caller can include them
/// in this node's own outbound announces from the very first one — see
/// `main.rs`'s call site.
pub async fn start(peers: PeerTable, config: DhtConfig) -> DhtHandle {
    start_with_node_http(
        peers,
        config,
        NodeHttpSettings::from_env().unwrap_or_default(),
    )
    .await
}

/// [`start`] with explicit node-to-node stream limits instead of the environment's.
pub async fn start_with_node_http(
    peers: PeerTable,
    config: DhtConfig,
    node_http: NodeHttpSettings,
) -> DhtHandle {
    let external_addr = config.external_addr.clone();
    let reachability = ReachabilityHandle::new(external_addr.as_ref().map(|a| a.to_string()));
    let bootstrap_scan_interval = config.bootstrap_scan_interval;
    let expected_identify_version = identify_protocol_version(&config.network_id);
    let mut swarm = build_swarm(
        config.identity,
        &config.network_id,
        &config.autonat,
        &config.relay,
        &node_http,
    );
    let local_peer_id = *swarm.local_peer_id();
    swarm.behaviour_mut().kad.set_mode(Some(Mode::Server));
    swarm
        .listen_on(config.listen_addr.clone())
        .unwrap_or_else(|e| panic!("avalon-dht: failed to bind {}: {e}", config.listen_addr));

    let mut listen_addrs = Vec::new();
    let collection_deadline = tokio::time::sleep(INITIAL_LISTEN_COLLECTION_WINDOW);
    tokio::pin!(collection_deadline);
    loop {
        tokio::select! {
            _ = &mut collection_deadline => break,
            event = swarm.select_next_some() => {
                if let SwarmEvent::NewListenAddr { address, .. } = event {
                    tracing::info!(%address, peer_id = %local_peer_id, "avalon-dht: listening");
                    listen_addrs.push(address);
                }
            }
        }
    }

    if listen_addrs.is_empty() {
        tracing::warn!(
            "avalon-dht: no listen address observed within {:?}",
            INITIAL_LISTEN_COLLECTION_WINDOW
        );
    }
    let relay_stats = RelayServerStats::new();
    let serves_relay = config.relay.server.is_some();
    if let Some(settings) = &config.relay.server {
        reachability.set_relay_server(crate::relay::RelayServerView {
            settings: settings.clone(),
            stats: relay_stats.clone(),
        });
    }
    if serves_relay {
        // A relay hands its external address to clients in reservation replies.
        if let Some(addr) = &external_addr {
            swarm.add_external_address(addr.clone());
        }
        set_relay_server_status(&mut swarm, Reachability::Unknown, external_addr.is_some());
    }
    if let (Some(addr), Some(autonat)) = (
        external_addr.clone(),
        swarm.behaviour_mut().autonat.as_mut(),
    ) {
        autonat.probe_address(addr);
    }
    let relay = RelayRuntime {
        client: RelayClient::new(
            config.relay.client.clone(),
            local_peer_id,
            reachability.clone(),
        ),
        stats: relay_stats.clone(),
        serves: serves_relay,
        has_external_addr: external_addr.is_some(),
        reconcile_interval: config.relay.client.reconcile_interval,
    };

    let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let router_slot = RouterSlot::new();
    let inbound = NodeHttpInbound::new(
        InboundService::new(router_slot.clone(), node_http, peers.clone()),
        node_http.max_inflight,
        node_http.timeout,
    );
    let stream_peers = peers.clone();
    tokio::spawn(run_worker(
        swarm,
        peers,
        commands_rx,
        bootstrap_scan_interval,
        expected_identify_version,
        reachability.clone(),
        relay,
        inbound,
    ));

    DhtHandle {
        peer_id: local_peer_id,
        listen_addrs,
        commands: commands_tx.clone(),
        reachability,
        relay_stats,
        router_slot,
        node_http: StreamHandle {
            commands: commands_tx,
            peers: Some(stream_peers),
            settings: node_http,
        },
    }
}

type HttpResponder = oneshot::Sender<Result<NodeHttpResponse, NodeHttpError>>;
type HttpAnswer = (ResponseChannel<NodeHttpResponse>, NodeHttpResponse);

/// Inbound stream requests: the service that answers them and the queue that returns finished
/// answers to the worker, which owns the swarm and so must send every response.
struct NodeHttpInbound {
    service: InboundService,
    answers: mpsc::Sender<HttpAnswer>,
    finished: mpsc::Receiver<HttpAnswer>,
    /// Bound on dialing a peer for a stream request.
    timeout: Duration,
}

impl NodeHttpInbound {
    /// `capacity` is the in-flight ceiling, so a finished answer never waits for queue room.
    fn new(service: InboundService, capacity: usize, timeout: Duration) -> Self {
        let (answers, finished) = mpsc::channel(capacity.max(1));
        Self {
            service,
            answers,
            finished,
            timeout,
        }
    }

    /// Answers a 429 at once when over a limit, else serves it on its own task.
    fn accept(
        &self,
        swarm: &mut Swarm<DhtBehaviour>,
        peer: PeerId,
        request: NodeHttpRequest,
        channel: ResponseChannel<NodeHttpResponse>,
    ) {
        let permit: InboundPermit = match self.service.admit(peer, &request) {
            Ok(p) => p,
            Err(refusal) => {
                let _ = swarm
                    .behaviour_mut()
                    .node_http
                    .send_response(channel, refusal);
                return;
            }
        };
        let (service, answers) = (self.service.clone(), self.answers.clone());
        // Keeps the body's buffer budget reserved until the answer is queued.
        let grant = request.grant.clone();
        tokio::spawn(async move {
            let response = service.handle(peer, request).await;
            let _ = answers.send((channel, response)).await;
            drop((permit, grant));
        });
    }
}

/// A stream request waiting for a connection to its peer.
struct ParkedHttp {
    request: NodeHttpRequest,
    respond_to: HttpResponder,
    deadline: Instant,
}

/// Requests parked per peer, and whether this queue has dialed that peer itself yet.
#[derive(Default)]
struct ParkedPeer {
    waiting: Vec<ParkedHttp>,
    /// The dial this queue started, so only its failure fails the queue.
    own_dial: Option<ConnectionId>,
}

/// Most requests parked behind one peer's dial.
const MAX_PARKED_PER_PEER: usize = 32;

type ParkedRequests = HashMap<PeerId, ParkedPeer>;

fn connect_failure(message: &str) -> NodeHttpError {
    NodeHttpError::Stream {
        kind: StreamErrorKind::Connect,
        message: message.to_string(),
    }
}

#[derive(PartialEq, Eq)]
enum HttpDial {
    /// This queue started a dial with this connection id.
    Started(ConnectionId),
    /// Another dial to the peer is already in flight.
    Joined,
    Failed,
}

/// Dials `peer` with the peer table's addresses (direct first) unless a dial is already in
/// flight.
fn dial_for_http(swarm: &mut Swarm<DhtBehaviour>, peers: &PeerTable, peer: PeerId) -> HttpDial {
    let opts = DialOpts::peer_id(peer)
        .addresses(table_addrs(peers, &peer))
        .extend_addresses_through_behaviour()
        .condition(PeerCondition::DisconnectedAndNotDialing)
        .build();
    let id = opts.connection_id();
    match swarm.dial(opts) {
        Ok(()) => HttpDial::Started(id),
        Err(DialError::DialPeerConditionFalse(_)) => HttpDial::Joined,
        Err(e) => {
            tracing::debug!(%peer, "avalon-dht: node-http dial not started: {e}");
            HttpDial::Failed
        }
    }
}

/// Sends `request` to a connected `peer` and tracks its response.
fn send_http(
    swarm: &mut Swarm<DhtBehaviour>,
    pending: &mut HashMap<OutboundRequestId, HttpResponder>,
    peer: PeerId,
    request: NodeHttpRequest,
    respond_to: HttpResponder,
) {
    let id =
        swarm
            .behaviour_mut()
            .node_http
            .send_request_with_addresses(&peer, request, Vec::new());
    pending.insert(id, respond_to);
}

/// Sends `request` now if `peer` is connected, else parks it behind a dial.
fn start_http(
    swarm: &mut Swarm<DhtBehaviour>,
    peers: &PeerTable,
    pending: &mut HashMap<OutboundRequestId, HttpResponder>,
    parked: &mut ParkedRequests,
    peer: PeerId,
    parked_request: ParkedHttp,
) {
    if swarm.is_connected(&peer) {
        send_http(
            swarm,
            pending,
            peer,
            parked_request.request,
            parked_request.respond_to,
        );
        return;
    }
    let entry = parked.entry(peer).or_default();
    if entry.waiting.len() >= MAX_PARKED_PER_PEER {
        let _ = parked_request.respond_to.send(Err(connect_failure(
            "too many requests waiting for the peer",
        )));
        return;
    }
    if entry.own_dial.is_none() {
        match dial_for_http(swarm, peers, peer) {
            HttpDial::Started(id) => entry.own_dial = Some(id),
            HttpDial::Joined => {}
            HttpDial::Failed => {
                let entry = parked.remove(&peer).unwrap_or_default();
                for w in entry.waiting.into_iter().chain([parked_request]) {
                    let _ = w
                        .respond_to
                        .send(Err(connect_failure("no address to dial the peer")));
                }
                return;
            }
        }
    }
    entry.waiting.push(parked_request);
}

/// A connection to `peer` is up: send everything parked for it.
fn flush_parked(
    swarm: &mut Swarm<DhtBehaviour>,
    pending: &mut HashMap<OutboundRequestId, HttpResponder>,
    parked: &mut ParkedRequests,
    peer: PeerId,
) {
    for w in parked.remove(&peer).unwrap_or_default().waiting {
        send_http(swarm, pending, peer, w.request, w.respond_to);
    }
}

/// The dial `failed` to `peer` ended in an error: if it was this queue's own, fail what is
/// parked; if it was someone else's, try once with this queue's own addresses.
fn parked_dial_failed(
    swarm: &mut Swarm<DhtBehaviour>,
    peers: &PeerTable,
    parked: &mut ParkedRequests,
    peer: PeerId,
    failed: ConnectionId,
) {
    let Some(entry) = parked.get_mut(&peer) else {
        return;
    };
    if swarm.is_connected(&peer) || entry.own_dial.is_some_and(|own| own != failed) {
        return;
    }
    if entry.own_dial.is_none() {
        match dial_for_http(swarm, peers, peer) {
            HttpDial::Started(id) => {
                entry.own_dial = Some(id);
                return;
            }
            HttpDial::Joined => return,
            HttpDial::Failed => {}
        }
    }
    for w in parked.remove(&peer).unwrap_or_default().waiting {
        let _ = w
            .respond_to
            .send(Err(connect_failure("failed to dial the peer")));
    }
}

/// The peer of a new connection at an address the policy allows: only those may carry
/// parked requests, since the others are disconnected right away.
fn flushable_connection(event: &SwarmEvent<DhtBehaviourEvent>) -> Option<PeerId> {
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } if endpoint_ip_allowed(endpoint.get_remote_address()) => Some(*peer_id),
        _ => None,
    }
}

/// Dials `addr` for the bootstrap scan. `true` when the peer may be marked known: the dial
/// started, or it is connected already; `false` when another dial is in flight, so a failure of
/// that dial does not strand the peer.
fn scan_dial(swarm: &mut Swarm<DhtBehaviour>, peer_id: PeerId, addr: Multiaddr) -> bool {
    // Peer-aware, so a stream request's own dial to the same peer joins this one instead of
    // racing it.
    let opts = DialOpts::peer_id(peer_id)
        .addresses(vec![addr])
        .condition(PeerCondition::DisconnectedAndNotDialing)
        .build();
    match swarm.dial(opts) {
        Ok(()) => true,
        Err(DialError::DialPeerConditionFalse(_)) => swarm.is_connected(&peer_id),
        Err(e) => {
            tracing::warn!(%peer_id, "avalon-dht: dial failed: {e}");
            true
        }
    }
}

/// Drops parked requests whose caller gave up and fails those past their deadline.
fn expire_parked(parked: &mut ParkedRequests, now: Instant) {
    for entry in parked.values_mut() {
        entry.waiting.retain_mut(|w| {
            if w.respond_to.is_closed() {
                return false;
            }
            if now >= w.deadline {
                let timeout = NodeHttpError::Stream {
                    kind: StreamErrorKind::Timeout,
                    message: "timed out dialing the peer".to_string(),
                };
                // The responder moves out only when sent; replace with a closed one.
                let (dead, _) = oneshot::channel();
                let real = std::mem::replace(&mut w.respond_to, dead);
                let _ = real.send(Err(timeout));
                return false;
            }
            true
        });
    }
    parked.retain(|_, e| !e.waiting.is_empty());
}

/// Maps a stream failure to the error call sites see.
fn node_http_error(failure: request_response::OutboundFailure) -> NodeHttpError {
    use request_response::OutboundFailure as F;
    let kind = match &failure {
        F::Timeout => StreamErrorKind::Timeout,
        F::DialFailure | F::ConnectionClosed | F::UnsupportedProtocols => StreamErrorKind::Connect,
        F::Io(_) => StreamErrorKind::Protocol,
    };
    NodeHttpError::Stream {
        kind,
        message: failure.to_string(),
    }
}

/// Candidate addresses for `peer` from the peer table, direct before relayed.
fn table_addrs(peers: &PeerTable, peer: &PeerId) -> Vec<Multiaddr> {
    peers
        .list_all()
        .into_iter()
        .chain(peers.list_unverified())
        .find_map(|info| {
            let (id, addrs) = new_dht_peer(&info, &HashSet::new())?;
            (id == *peer).then_some(addrs)
        })
        .unwrap_or_default()
}

/// The relay pieces the worker drives.
struct RelayRuntime {
    client: RelayClient,
    stats: Arc<RelayServerStats>,
    serves: bool,
    has_external_addr: bool,
    reconcile_interval: Duration,
}

/// Advertises the relay hop protocol only while this node is dialable: detected `public`, or
/// undetected with an operator-stated external address. A `private` node never relays.
fn set_relay_server_status(
    swarm: &mut Swarm<DhtBehaviour>,
    reachability: Reachability,
    has_external_addr: bool,
) {
    let dialable = match reachability {
        Reachability::Public => true,
        Reachability::Unknown => has_external_addr,
        Reachability::Private => false,
    };
    if let Some(server) = swarm.behaviour_mut().relay_server.as_mut() {
        server.set_status(Some(if dialable {
            relay::Status::Enable
        } else {
            relay::Status::Disable
        }));
    }
}

/// Parses `info`'s DHT identity (if any) into a dialable `(PeerId,
/// Vec<Multiaddr>)`, skipping peers already in `known` — pure and
/// unit-testable independent of a real swarm, same "pure function behind
/// the real worker" split `crate::nodes::resolve_bootstrap_peers` already
/// established. A peer with an unparseable `libp2p_peer_id`/address (never
/// expected from this node's own `nodes::announce_to`, but this table can
/// also be populated by a misbehaving or buggy remote peer) is skipped
/// rather than treated as fatal — the same "don't let one bad peer take
/// down peer processing" posture `admit_if_supported` already takes for a
/// version-floor mismatch.
fn new_dht_peer(info: &PeerInfo, known: &HashSet<PeerId>) -> Option<(PeerId, Vec<Multiaddr>)> {
    let peer_id: PeerId = info.libp2p_peer_id.as_ref()?.parse().ok()?;
    if known.contains(&peer_id) {
        return None;
    }
    let addrs: Vec<Multiaddr> = info
        .libp2p_listen_addrs
        .iter()
        .filter_map(|a| a.parse().ok())
        .collect();
    Some((peer_id, direct_first(addrs)))
}

/// How long the higher peer id waits for the lower one to dial a relayed-only peer first.
const RELAYED_DIAL_GRACE: Duration = Duration::from_secs(20);

/// Whether to dial a peer reachable only through a relay now. Two nodes dialing each other's
/// circuit at once open crossed relayed connections and run DCUtR twice with opposite roles,
/// which breaks the simultaneous open; so the lower peer id dials first and the other only
/// dials if that has not produced a connection within [`RELAYED_DIAL_GRACE`].
fn relayed_dial_due(
    first_seen: &mut HashMap<PeerId, Instant>,
    local: &PeerId,
    remote: PeerId,
    now: Instant,
) -> bool {
    if local.to_bytes() < remote.to_bytes() {
        return true;
    }
    let first = *first_seen.entry(remote).or_insert(now);
    now.duration_since(first) >= RELAYED_DIAL_GRACE
}

fn is_relayed_addr(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::P2pCircuit))
}

/// Unverified-pool peers dialed per bootstrap scan.
const MAX_POOL_DIALS_PER_SCAN: usize = 4;

/// Direct addresses before relayed ones, each group in its original order.
fn direct_first(addrs: Vec<Multiaddr>) -> Vec<Multiaddr> {
    let (relayed, direct): (Vec<_>, Vec<_>) = addrs.into_iter().partition(is_relayed_addr);
    direct.into_iter().chain(relayed).collect()
}

/// Kademlia serves queries unless AutoNAT found this node unreachable.
fn kad_mode_for(reachability: Reachability) -> kad::Mode {
    match reachability {
        Reachability::Private => Mode::Client,
        Reachability::Public | Reachability::Unknown => Mode::Server,
    }
}

fn endpoint_ip_allowed(addr: &Multiaddr) -> bool {
    addr.iter().all(|p| match p {
        Protocol::Ip4(ip) => peer_ip_allowed(ip.into()),
        Protocol::Ip6(ip) => peer_ip_allowed(ip.into()),
        _ => true,
    })
}

/// Publishes AutoNAT status changes and confirmed external addresses into `reachability`.
fn track_reachability(
    swarm: &mut Swarm<DhtBehaviour>,
    reachability: &ReachabilityHandle,
    event: &SwarmEvent<DhtBehaviourEvent>,
) {
    match event {
        SwarmEvent::Behaviour(DhtBehaviourEvent::Autonat(autonat::Event::StatusChanged {
            old,
            new,
        })) => {
            tracing::info!(?old, ?new, "avalon-dht: AutoNAT reachability changed");
            let detected = Reachability::from_nat_status(new);
            reachability.set_reachability(detected);
            swarm
                .behaviour_mut()
                .kad
                .set_mode(Some(kad_mode_for(detected)));
            if detected == Reachability::Private {
                let stale: Vec<Multiaddr> = swarm.external_addresses().cloned().collect();
                for addr in stale {
                    swarm.remove_external_address(&addr);
                }
            }
        }
        SwarmEvent::ExternalAddrConfirmed { address }
            if !address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) =>
        {
            tracing::info!(%address, "avalon-dht: external address confirmed");
            reachability.confirm_addr(address.to_string());
        }
        SwarmEvent::ExternalAddrExpired { address } => {
            reachability.expire_addr(&address.to_string());
        }
        _ => {}
    }
}

/// Records each hole-punch outcome and which punched connections are still open. A failed
/// punch changes nothing: the relayed connection it started from stays in use.
fn track_hole_punch(
    punched: &mut HashMap<ConnectionId, PeerId>,
    reachability: &ReachabilityHandle,
    event: &SwarmEvent<DhtBehaviourEvent>,
) {
    match event {
        SwarmEvent::Behaviour(DhtBehaviourEvent::Dcutr(outcome)) => {
            let peer = outcome.remote_peer_id;
            let error = match &outcome.result {
                Ok(connection) => {
                    tracing::info!(%peer, "avalon-dht: hole punch succeeded, using a direct connection");
                    punched.insert(*connection, peer);
                    None
                }
                Err(e) => {
                    tracing::info!(%peer, "avalon-dht: hole punch failed, staying on the relay: {e}");
                    Some(e.to_string())
                }
            };
            reachability.record_hole_punch(HolePunchOutcome {
                peer_id: peer.to_string(),
                succeeded: error.is_none(),
                error,
            });
        }
        SwarmEvent::ConnectionClosed { connection_id, .. } => {
            if punched.remove(connection_id).is_none() {
                return;
            }
        }
        _ => return,
    }
    let mut peers: Vec<String> = punched.values().map(|p| p.to_string()).collect();
    peers.sort();
    peers.dedup();
    reachability.set_punched_peers(peers);
}

/// Feeds relay-related events to the relay client and stats. `true` when the event was
/// consumed by relay handling alone.
fn handle_relay_event(
    swarm: &mut Swarm<DhtBehaviour>,
    relay: &mut RelayRuntime,
    reachability: &ReachabilityHandle,
    event: &SwarmEvent<DhtBehaviourEvent>,
) -> bool {
    let now = Instant::now();
    let detected = || reachability.snapshot().reachability;
    match event {
        SwarmEvent::Behaviour(DhtBehaviourEvent::Autonat(autonat::Event::StatusChanged {
            ..
        })) => {
            if relay.serves {
                set_relay_server_status(swarm, detected(), relay.has_external_addr);
            }
            relay.client.reconcile(swarm, detected(), now);
            false
        }
        SwarmEvent::Behaviour(DhtBehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) => {
            if info.protocols.contains(&relay::HOP_PROTOCOL_NAME) {
                relay
                    .client
                    .note_hop_relay(*peer_id, info.listen_addrs.clone());
                relay.client.reconcile(swarm, detected(), now);
            }
            false
        }
        SwarmEvent::Behaviour(DhtBehaviourEvent::RelayClient(
            relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal,
                ..
            },
        )) => {
            relay.client.on_accepted(*relay_peer_id, *renewal);
            true
        }
        SwarmEvent::ListenerClosed { listener_id, .. } => {
            relay.client.on_listener_closed(*listener_id, now);
            relay.client.reconcile(swarm, detected(), now);
            false
        }
        SwarmEvent::Behaviour(DhtBehaviourEvent::RelayServer(server_event)) => {
            relay.stats.record(server_event);
            true
        }
        _ => false,
    }
}

fn handle_node_http_event(
    swarm: &mut Swarm<DhtBehaviour>,
    inbound: &NodeHttpInbound,
    pending: &mut HashMap<OutboundRequestId, HttpResponder>,
    event: request_response::Event<NodeHttpRequest, NodeHttpResponse>,
) {
    use request_response::{Event, Message};
    match event {
        Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => inbound.accept(swarm, peer, request, channel),
        Event::Message {
            message:
                Message::Response {
                    request_id,
                    response,
                },
            ..
        } => {
            if let Some(respond_to) = pending.remove(&request_id) {
                let _ = respond_to.send(Ok(response));
            }
        }
        Event::OutboundFailure {
            request_id, error, ..
        } => {
            if let Some(respond_to) = pending.remove(&request_id) {
                let _ = respond_to.send(Err(node_http_error(error)));
            }
        }
        Event::InboundFailure { peer, error, .. } => {
            tracing::debug!(%peer, "avalon-dht: node-http inbound request failed: {error}");
        }
        Event::ResponseSent { .. } => {}
    }
}

/// Never returns. Handles `identify` responses (feeding a directly-dialed
/// peer's own reported listen addresses into `kad` — without this, a fresh
/// connection never actually populates the DHT routing table), on
/// `bootstrap_scan_interval` scans `peers` for any
/// DHT identity not yet dialed, and services [`DhtCommand`]s from
/// `commands`.
///
/// `expected_identify_version` is this node's own
/// `identify_protocol_version(network_id)`. A peer whose reported
/// `protocol_version` doesn't match it is immediately disconnected —
/// defense in depth on top of [`kad_protocol_name`]'s own hard protocol-id
/// mismatch (which already prevents any Kademlia RPC from working between
/// differently-networked swarms regardless of this check): a mismatched
/// peer is dropped outright here rather than left connected-but-useless.
#[allow(clippy::too_many_arguments)]
async fn run_worker(
    mut swarm: Swarm<DhtBehaviour>,
    peers: PeerTable,
    mut commands: mpsc::Receiver<DhtCommand>,
    bootstrap_scan_interval: Duration,
    expected_identify_version: String,
    reachability: ReachabilityHandle,
    mut relay: RelayRuntime,
    mut inbound: NodeHttpInbound,
) {
    let mut pending_http: HashMap<OutboundRequestId, HttpResponder> = HashMap::new();
    let mut parked_http = ParkedRequests::new();
    let connect_timeout = inbound.timeout;
    let mut reconcile_tick = tokio::time::interval(relay.reconcile_interval);
    let mut known_peers: HashSet<PeerId> = HashSet::new();
    let mut punched: HashMap<ConnectionId, PeerId> = HashMap::new();
    let mut relayed_first_seen: HashMap<PeerId, Instant> = HashMap::new();
    let local_peer_id = *swarm.local_peer_id();
    let mut scan_interval = tokio::time::interval(bootstrap_scan_interval);
    // The first tick fires immediately; bootstrap from whatever the peer
    // table already knows about right away rather than waiting a full interval.

    // One entry per in-flight `get_record` query, accumulating
    // `FoundRecord` values until `ProgressStep::last` says the query is
    // done — a `get_record` can (and usually does) yield more than one
    // `OutboundQueryProgressed` event before it finishes.
    let mut pending_gets: HashMap<QueryId, PendingGet> = HashMap::new();

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    // The sender half (`DhtHandle::commands`) was dropped —
                    // only happens if the whole process is tearing down,
                    // since `AppState`/`main.rs` hold it for the process
                    // lifetime. Nothing left to service; end the worker
                    // rather than spin on a closed channel forever.
                    return;
                };
                match command {
                    DhtCommand::PutRecord { key, value, ttl } => {
                        // A solo node (no known DHT peers yet — the common
                        // case right after startup, or a genuinely small
                        // network) can never satisfy `Quorum::One`: that
                        // quorum counts *other* peers, not this node's own
                        // local store (referenced below).
                        // Every put would be a guaranteed, immediate
                        // failure — for `identity_locator`'s callers alone
                        // that's one doomed query per known identity, every
                        // `interest::REFRESH_INTERVAL`, which in a
                        // long-lived dev database with hundreds of
                        // identities and no connected peers turns into a
                        // continuous stream of failing queries burning CPU
                        // for no possible benefit. Skipping here is exactly
                        // equivalent to the query failing instantly, just
                        // without paying for it — normal puts resume the
                        // moment any peer is known.
                        if known_peers.is_empty() {
                            continue;
                        }
                        let record = kad::Record {
                            key: key.into(),
                            value,
                            publisher: None,
                            expires: Some(Instant::now() + ttl),
                        };
                        if let Err(e) = swarm.behaviour_mut().kad.put_record(record, kad::Quorum::One) {
                            tracing::warn!("avalon-dht: put_record rejected locally: {e}");
                        }
                    }
                    DhtCommand::GetRecord { key, respond_to } => {
                        let query_id = swarm.behaviour_mut().kad.get_record(key.into());
                        pending_gets.insert(query_id, (respond_to, Vec::new()));
                    }
                    DhtCommand::HttpRequest { peer, request, respond_to } => {
                        // Callers that gave up leave a closed responder behind; drop those first.
                        pending_http.retain(|_, r| !r.is_closed());
                        expire_parked(&mut parked_http, Instant::now());
                        let parked = ParkedHttp {
                            request,
                            respond_to,
                            deadline: Instant::now() + connect_timeout,
                        };
                        start_http(&mut swarm, &peers, &mut pending_http, &mut parked_http, peer, parked);
                    }
                }
            }
            Some((channel, response)) = inbound.finished.recv() => {
                let _ = swarm.behaviour_mut().node_http.send_response(channel, response);
            }
            _ = scan_interval.tick() => {
                expire_parked(&mut parked_http, Instant::now());
                // Gossip-learned peers that cannot be reached over HTTP (a relayed node has no
                // reachable URL) sit in the unverified pool; dial a few per scan so a libp2p
                // connection can confirm them.
                let pooled: Vec<PeerInfo> = peers
                    .list_unverified()
                    .into_iter()
                    .filter(|info| {
                        new_dht_peer(info, &known_peers).is_some_and(|(_, a)| !a.is_empty())
                    })
                    .take(MAX_POOL_DIALS_PER_SCAN)
                    .collect();
                for info in peers.list_all().into_iter().chain(pooled) {
                    if let Some((peer_id, addrs)) = new_dht_peer(&info, &known_peers) {
                        if !addrs.is_empty()
                            && addrs.iter().all(is_relayed_addr)
                            && !relayed_dial_due(&mut relayed_first_seen, &local_peer_id, peer_id, Instant::now())
                        {
                            continue;
                        }
                        if addrs.is_empty() {
                            // Known to the network but not yet dialable — try
                            // again on a later scan once it reports a real
                            // address; not added to `known_peers` so this
                            // isn't a one-shot miss.
                            continue;
                        }
                        tracing::info!(%peer_id, base_url = %info.base_url, "avalon-dht: bootstrapping peer from peer table");
                        for addr in &addrs {
                            swarm.behaviour_mut().kad.add_address(&peer_id, addr.clone());
                        }
                        if let Some(addr) = addrs.into_iter().next() {
                            if !scan_dial(&mut swarm, peer_id, addr) {
                                continue;
                            }
                        }
                        known_peers.insert(peer_id);
                    }
                }
            }
            _ = reconcile_tick.tick() => {
                relay.client.reconcile(&mut swarm, reachability.snapshot().reachability, Instant::now());
            }
            event = swarm.select_next_some() => {
                track_reachability(&mut swarm, &reachability, &event);
                track_hole_punch(&mut punched, &reachability, &event);
                if handle_relay_event(&mut swarm, &mut relay, &reachability, &event) {
                    continue;
                }
                if let Some(peer_id) = flushable_connection(&event) {
                    flush_parked(&mut swarm, &mut pending_http, &mut parked_http, peer_id);
                }
                if let SwarmEvent::OutgoingConnectionError {
                    peer_id: Some(peer_id),
                    connection_id,
                    ..
                } = &event
                {
                    parked_dial_failed(
                        &mut swarm,
                        &peers,
                        &mut parked_http,
                        *peer_id,
                        *connection_id,
                    );
                }
                if let SwarmEvent::Behaviour(DhtBehaviourEvent::NodeHttp(http_event)) = event {
                    handle_node_http_event(&mut swarm, &inbound, &mut pending_http, http_event);
                } else if let SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } = event {
                    if endpoint.is_dialer() {
                        for url in peers.promote_unverified_by_libp2p_peer(
                            &peer_id.to_string(),
                            crate::peer_admission::admission().cfg.max_known_peers,
                        ) {
                            tracing::info!(
                                event = "peer_promoted_from_unverified_pool",
                                peer = %url,
                                %peer_id,
                                "promoted a gossip-learned peer after an authenticated libp2p connection",
                            );
                        }
                    }
                    if !endpoint_ip_allowed(endpoint.get_remote_address()) {
                        tracing::warn!(%peer_id, "avalon-dht: disconnecting peer at an address the outbound policy always refuses");
                        let _ = swarm.disconnect_peer_id(peer_id);
                        continue;
                    }
                    // Seed the routing table from the connection's own
                    // address immediately —
                    // otherwise a peer dialed directly (not yet known to
                    // `kad`) can't be looked up until `identify` completes
                    // its own round trip.
                    swarm
                        .behaviour_mut()
                        .kad
                        .add_address(&peer_id, endpoint.get_remote_address().clone());
                    known_peers.insert(peer_id);
                } else if let SwarmEvent::OutgoingConnectionError { peer_id, error, .. } = event {
                    // Otherwise a dial that fails asynchronously (bad
                    // multiaddr, handshake mismatch, unreachable host)
                    // fails completely silently — the scan branch above
                    // only logs the *attempt*, never its outcome. Not
                    // removed from `known_peers`: a persistently
                    // unreachable peer just doesn't get retried until it
                    // re-announces (matching the peer table's
                    // pruning-based recovery, not an independent retry
                    // policy here).
                    tracing::warn!(?peer_id, "avalon-dht: outgoing connection failed: {error}");
                } else if let SwarmEvent::Behaviour(DhtBehaviourEvent::Identify(
                    identify::Event::Received { peer_id, info, .. },
                )) = event
                {
                    if info.protocol_version != expected_identify_version {
                        // A peer identifying for a different
                        // network — its own kad protocol id already
                        // couldn't negotiate a single RPC with this swarm
                        // (see `kad_protocol_name`'s own doc comment), but
                        // disconnect explicitly rather than leave a
                        // connected-but-useless peer sitting in this node's
                        // connection table indefinitely.
                        tracing::warn!(
                            %peer_id,
                            expected = %expected_identify_version,
                            got = %info.protocol_version,
                            "avalon-dht: peer identified for a different network — disconnecting"
                        );
                        let _ = swarm.disconnect_peer_id(peer_id);
                        continue;
                    }
                    for addr in info.listen_addrs {
                        swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                    }
                } else if let SwarmEvent::Behaviour(DhtBehaviourEvent::Kad(
                    kad::Event::OutboundQueryProgressed { id, result, step, .. },
                )) = event
                {
                    match result {
                        kad::QueryResult::PutRecord(Err(e)) => {
                            // Fire-and-forget from the caller's perspective
                            // (`crate::interest` just re-puts on its own
                            // refresh timer) — nowhere else to surface this
                            // but a log. Expected in a small network:
                            // quorum counts *other*
                            // peers, not this node's own local store.
                            tracing::warn!("avalon-dht: put_record failed: {e}");
                        }
                        kad::QueryResult::GetRecord(result) => {
                            let done = step.last;
                            if let Some((_, values)) = pending_gets.get_mut(&id) {
                                if let Ok(kad::GetRecordOk::FoundRecord(found)) = result {
                                    values.push(found.record.value);
                                }
                            }
                            if done {
                                if let Some((respond_to, values)) = pending_gets.remove(&id) {
                                    // The receiver may already be gone if
                                    // the caller stopped waiting (e.g. its
                                    // own timeout) — nothing to do then.
                                    let _ = respond_to.send(values);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::core::ConnectedPoint;

    fn closed(peer: PeerId, connection: ConnectionId) -> SwarmEvent<DhtBehaviourEvent> {
        SwarmEvent::ConnectionClosed {
            peer_id: peer,
            connection_id: connection,
            endpoint: ConnectedPoint::Dialer {
                address: "/ip4/203.0.113.7/tcp/4001".parse().unwrap(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            },
            num_established: 0,
            cause: None,
        }
    }

    #[test]
    fn punched_connections_are_tracked_until_they_close() {
        let handle = ReachabilityHandle::unknown();
        let mut punched = HashMap::new();
        let (peer, other) = (PeerId::random(), PeerId::random());
        let direct = ConnectionId::new_unchecked(7);

        track_hole_punch(
            &mut punched,
            &handle,
            &SwarmEvent::Behaviour(DhtBehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id: peer,
                result: Ok(direct),
            })),
        );
        let snap = handle.snapshot();
        assert_eq!(snap.punched_peers, vec![peer.to_string()]);
        assert!(snap.hole_punches.last().unwrap().succeeded);

        // Another connection closing changes nothing.
        track_hole_punch(
            &mut punched,
            &handle,
            &closed(other, ConnectionId::new_unchecked(8)),
        );
        assert_eq!(handle.snapshot().punched_peers, vec![peer.to_string()]);

        track_hole_punch(&mut punched, &handle, &closed(peer, direct));
        assert!(handle.snapshot().punched_peers.is_empty());
        assert_eq!(
            handle.snapshot().hole_punches.len(),
            1,
            "the outcome stays recorded"
        );
    }

    #[test]
    fn a_peer_with_two_punched_connections_stays_punched_until_both_close() {
        let handle = ReachabilityHandle::unknown();
        let mut punched = HashMap::new();
        let peer = PeerId::random();
        let (first, second) = (
            ConnectionId::new_unchecked(1),
            ConnectionId::new_unchecked(2),
        );
        for connection in [first, second] {
            track_hole_punch(
                &mut punched,
                &handle,
                &SwarmEvent::Behaviour(DhtBehaviourEvent::Dcutr(dcutr::Event {
                    remote_peer_id: peer,
                    result: Ok(connection),
                })),
            );
        }
        assert_eq!(handle.snapshot().punched_peers, vec![peer.to_string()]);
        track_hole_punch(&mut punched, &handle, &closed(peer, first));
        assert_eq!(handle.snapshot().punched_peers, vec![peer.to_string()]);
        track_hole_punch(&mut punched, &handle, &closed(peer, second));
        assert!(handle.snapshot().punched_peers.is_empty());
    }

    fn test_swarm() -> Swarm<DhtBehaviour> {
        build_swarm(
            identity::Keypair::generate_ed25519(),
            "net",
            &AutonatSettings::default(),
            &RelaySettings::default(),
            &NodeHttpSettings::default(),
        )
    }

    fn parked_request() -> (
        ParkedHttp,
        oneshot::Receiver<Result<NodeHttpResponse, NodeHttpError>>,
    ) {
        let (respond_to, rx) = oneshot::channel();
        let request = NodeHttpRequest {
            method: "GET".into(),
            path_and_query: "/nodes/status".into(),
            headers: vec![],
            body: vec![],
            grant: Default::default(),
        };
        (
            ParkedHttp {
                request,
                respond_to,
                deadline: Instant::now() + Duration::from_secs(60),
            },
            rx,
        )
    }

    #[tokio::test]
    async fn only_the_queues_own_dial_failing_fails_its_parked_requests() {
        let mut swarm = test_swarm();
        let peer = sample_peer_id();
        let (own, other) = (
            ConnectionId::new_unchecked(1),
            ConnectionId::new_unchecked(2),
        );
        let (w, mut rx) = parked_request();
        let mut parked = ParkedRequests::new();
        parked.insert(
            peer,
            ParkedPeer {
                waiting: vec![w],
                own_dial: Some(own),
            },
        );
        let peers = PeerTable::new();
        parked_dial_failed(&mut swarm, &peers, &mut parked, peer, other);
        assert!(parked.contains_key(&peer));
        assert!(
            rx.try_recv().is_err(),
            "an unrelated dial must not fail the queue"
        );
        parked_dial_failed(&mut swarm, &peers, &mut parked, peer, own);
        assert!(parked.is_empty());
        assert!(matches!(
            rx.try_recv(),
            Ok(Err(NodeHttpError::Stream { .. }))
        ));
    }

    #[tokio::test]
    async fn the_parked_queue_per_peer_is_capped() {
        let mut swarm = test_swarm();
        let peer = sample_peer_id();
        let mut parked = ParkedRequests::new();
        let mut keep = Vec::new();
        let mut entry = ParkedPeer {
            own_dial: Some(ConnectionId::new_unchecked(1)),
            ..ParkedPeer::default()
        };
        for _ in 0..MAX_PARKED_PER_PEER {
            let (w, rx) = parked_request();
            entry.waiting.push(w);
            keep.push(rx);
        }
        parked.insert(peer, entry);
        let (extra, mut rx) = parked_request();
        start_http(
            &mut swarm,
            &PeerTable::new(),
            &mut HashMap::new(),
            &mut parked,
            peer,
            extra,
        );
        assert_eq!(parked[&peer].waiting.len(), MAX_PARKED_PER_PEER);
        assert!(matches!(
            rx.try_recv(),
            Ok(Err(NodeHttpError::Stream { .. }))
        ));
    }

    #[tokio::test]
    async fn a_scan_dial_skipped_for_an_inflight_dial_does_not_mark_the_peer_known() {
        let mut swarm = test_swarm();
        let peer = sample_peer_id();
        let addr: Multiaddr = "/ip4/203.0.113.7/tcp/4001".parse().unwrap();
        assert!(
            scan_dial(&mut swarm, peer, addr.clone()),
            "a started dial marks known"
        );
        assert!(
            !scan_dial(&mut swarm, peer, addr),
            "a dial already in flight must leave the peer for a later scan"
        );
    }

    #[test]
    fn only_connections_at_allowed_addresses_flush_parked_requests() {
        let established = |addr: &str| SwarmEvent::ConnectionEstablished {
            peer_id: PeerId::random(),
            connection_id: ConnectionId::new_unchecked(1),
            endpoint: libp2p::core::ConnectedPoint::Dialer {
                address: addr.parse().unwrap(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            },
            num_established: std::num::NonZeroU32::new(1).unwrap(),
            concurrent_dial_errors: None,
            established_in: Duration::ZERO,
        };
        assert!(flushable_connection(&established("/ip4/203.0.113.7/tcp/4001")).is_some());
        assert!(flushable_connection(&established("/ip4/169.254.169.254/tcp/4001")).is_none());
    }

    #[test]
    fn kad_protocol_name_differs_across_network_ids() {
        assert_ne!(
            kad_protocol_name("avalon-dev-local").as_ref(),
            kad_protocol_name("avalon-mainnet-1").as_ref()
        );
    }

    #[test]
    fn kad_protocol_name_is_deterministic_for_the_same_network_id() {
        assert_eq!(
            kad_protocol_name("avalon-dev-local").as_ref(),
            kad_protocol_name("avalon-dev-local").as_ref()
        );
    }

    #[test]
    fn identify_protocol_version_differs_across_network_ids() {
        assert_ne!(
            identify_protocol_version("avalon-dev-local"),
            identify_protocol_version("avalon-mainnet-1")
        );
    }

    fn peer_info(base_url: &str, peer_id: Option<&str>, addrs: Vec<&str>) -> PeerInfo {
        PeerInfo {
            identity_bound: false,
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: peer_id.map(|s| s.to_string()),
            libp2p_listen_addrs: addrs.into_iter().map(|s| s.to_string()).collect(),
            connectivity: None,
            witness: None,
        }
    }

    fn sample_peer_id() -> PeerId {
        identity::Keypair::generate_ed25519().public().into()
    }

    #[test]
    fn only_the_lower_peer_id_dials_a_relayed_peer_at_once() {
        let (a, b) = (sample_peer_id(), sample_peer_id());
        let (low, high) = if a.to_bytes() < b.to_bytes() {
            (a, b)
        } else {
            (b, a)
        };
        let mut seen = HashMap::new();
        let t0 = Instant::now();
        assert!(relayed_dial_due(&mut seen, &low, high, t0));
        assert!(!relayed_dial_due(&mut seen, &high, low, t0));
        assert!(!relayed_dial_due(
            &mut seen,
            &high,
            low,
            t0 + RELAYED_DIAL_GRACE / 2
        ));
        assert!(relayed_dial_due(
            &mut seen,
            &high,
            low,
            t0 + RELAYED_DIAL_GRACE
        ));
    }

    #[test]
    fn kad_runs_as_client_only_when_autonat_reports_private() {
        assert_eq!(kad_mode_for(Reachability::Private), Mode::Client);
        assert_eq!(kad_mode_for(Reachability::Public), Mode::Server);
        assert_eq!(kad_mode_for(Reachability::Unknown), Mode::Server);
    }

    #[test]
    fn candidate_addresses_are_ordered_direct_first_then_relayed() {
        let relay = sample_peer_id();
        let me = sample_peer_id();
        let relayed = format!("/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}");
        let info = peer_info(
            "http://a",
            Some(&me.to_string()),
            vec![
                &relayed,
                "/ip4/203.0.113.7/tcp/4001",
                "/ip4/203.0.113.8/tcp/4001",
            ],
        );
        let (_, addrs) = new_dht_peer(&info, &HashSet::new()).unwrap();
        let strs: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
        assert_eq!(
            strs,
            vec![
                "/ip4/203.0.113.7/tcp/4001",
                "/ip4/203.0.113.8/tcp/4001",
                &relayed
            ]
        );
    }

    #[test]
    fn a_peer_with_no_libp2p_identity_is_not_a_dht_candidate() {
        let info = peer_info("http://a", None, vec![]);
        assert!(new_dht_peer(&info, &HashSet::new()).is_none());
    }

    #[test]
    fn a_known_peer_is_not_returned_again() {
        let peer_id = sample_peer_id();
        let info = peer_info(
            "http://a",
            Some(&peer_id.to_string()),
            vec!["/ip4/127.0.0.1/tcp/4001"],
        );
        let mut known = HashSet::new();
        known.insert(peer_id);
        assert!(new_dht_peer(&info, &known).is_none());
    }

    #[test]
    fn a_new_peer_with_a_valid_identity_and_address_is_returned() {
        let peer_id = sample_peer_id();
        let info = peer_info(
            "http://a",
            Some(&peer_id.to_string()),
            vec!["/ip4/127.0.0.1/tcp/4001"],
        );
        let (found_peer_id, addrs) = new_dht_peer(&info, &HashSet::new()).unwrap();
        assert_eq!(found_peer_id, peer_id);
        assert_eq!(addrs.len(), 1);
    }

    #[test]
    fn an_unparseable_peer_id_is_skipped_not_fatal() {
        let info = peer_info("http://a", Some("not-a-real-peer-id"), vec![]);
        assert!(new_dht_peer(&info, &HashSet::new()).is_none());
    }

    #[test]
    fn unparseable_addresses_are_dropped_but_dont_block_the_peer() {
        let peer_id = sample_peer_id();
        let info = peer_info(
            "http://a",
            Some(&peer_id.to_string()),
            vec!["not-a-multiaddr", "/ip4/127.0.0.1/tcp/4001"],
        );
        let (_, addrs) = new_dht_peer(&info, &HashSet::new()).unwrap();
        assert_eq!(addrs.len(), 1);
    }

    #[test]
    fn dht_config_from_env_is_some_when_unset() {
        let _env = crate::test_env::guard();
        // Unset defaults to enabled, not disabled.
        // SAFETY-of-intent note: process-global env var, same posture
        // `crate::nodes`'s own `node_roles_defaults_to_combined_when_unset`
        // test already takes.
        unsafe {
            std::env::remove_var("AVALON_DHT_ENABLED");
        }
        assert!(DhtConfig::from_env("avalon-dev-local").unwrap().is_some());
    }

    #[test]
    fn dht_config_from_env_is_none_when_explicitly_disabled() {
        let _env = crate::test_env::guard();
        unsafe {
            std::env::set_var("AVALON_DHT_ENABLED", "false");
        }
        assert!(DhtConfig::from_env("avalon-dev-local").unwrap().is_none());

        unsafe {
            std::env::set_var("AVALON_DHT_ENABLED", "0");
        }
        assert!(DhtConfig::from_env("avalon-dev-local").unwrap().is_none());

        unsafe {
            std::env::remove_var("AVALON_DHT_ENABLED");
        }
    }
}
