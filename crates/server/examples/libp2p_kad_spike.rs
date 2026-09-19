//! Issue #581 spike: does a `rust-libp2p` Kademlia DHT round-trip actually
//! work between two real local processes, and what's the shape of standing
//! one up? Answers only that question — no interest-routing logic, no
//! integration with `crate::nodes`'s existing HTTP peer table. This is a
//! `dev-dependency`-only `examples/` binary on purpose: `libp2p` is not a
//! dependency of `avalon-server` itself yet, pending #580's later
//! sub-issues actually building on this.
//!
//! Run as two separate OS processes on the same machine (the epic's own
//! "prove a basic DHT put/get round-trip live" ask, not a single-process
//! simulation):
//!
//! ```text
//! # terminal 1 — listener, puts a record once it has a peer
//! cargo run -p avalon-server --example libp2p_kad_spike -- listen
//!
//! # terminal 2 — paste the "listening on ... with peer id ..." line printed
//! # by terminal 1 as this process's dial target
//! cargo run -p avalon-server --example libp2p_kad_spike -- dial /ip4/127.0.0.1/tcp/<port>/p2p/<peer-id>
//! ```
//!
//! Terminal 2 (the dialer) looks up the record terminal 1 put and prints it
//! once found — the actual round-trip proof.

use std::error::Error;
use std::time::Duration;

use libp2p::futures::StreamExt;
use libp2p::kad::{self, store::MemoryStore, Mode, QueryResult, Record};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, noise, tcp, yamux, Multiaddr, PeerId, SwarmBuilder};

const PROTOCOL_VERSION: &str = "/avalon-spike/kad/1.0.0";
const TEST_KEY: &[u8] = b"avalon-581-spike-key";
const TEST_VALUE: &[u8] = b"hello from the other node";

#[derive(NetworkBehaviour)]
struct SpikeBehaviour {
    kad: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let mode = std::env::args().nth(1).unwrap_or_default();
    let dial_target: Option<Multiaddr> = std::env::args().nth(2).map(|s| s.parse()).transpose()?;

    let keypair = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(keypair.public());
    println!("this node's peer id: {local_peer_id}");

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| SpikeBehaviour {
            kad: kad::Behaviour::new(local_peer_id, MemoryStore::new(local_peer_id)),
            identify: identify::Behaviour::new(identify::Config::new(
                PROTOCOL_VERSION.to_string(),
                key.public(),
            )),
        })?
        .build();

    swarm.behaviour_mut().kad.set_mode(Some(Mode::Server));
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    if let Some(addr) = &dial_target {
        println!("dialing {addr}");
        swarm.dial(addr.clone())?;
    }

    let mut put_done = false;
    let mut get_started = false;

    loop {
        let event = swarm.select_next_some().await;
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!("listening on {address} with peer id {local_peer_id}");
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                println!("connection established with {peer_id}");
                // Seed Kademlia's routing table with the address this
                // connection is actually on, immediately — waiting on the
                // `identify` exchange below to do it races put_record/
                // get_record against identify's own round trip (observed:
                // QuorumFailed/NotFound when this line was missing). #362's
                // real peer table has no bootstrap-node concept to reuse
                // here yet (that's #582's job).
                swarm
                    .behaviour_mut()
                    .kad
                    .add_address(&peer_id, endpoint.get_remote_address().clone());
                if mode == "listen" && !put_done {
                    let record = Record::new(TEST_KEY.to_vec(), TEST_VALUE.to_vec());
                    swarm
                        .behaviour_mut()
                        .kad
                        .put_record(record, kad::Quorum::One)?;
                    put_done = true;
                    println!("put_record submitted — waiting for confirmation");
                }
                if mode == "dial" && !get_started {
                    // Give the listener's put_record a moment to actually
                    // land before querying for it — a real interest-lookup
                    // caller (#583) will need its own retry/backoff for
                    // this, not assumed here.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    swarm
                        .behaviour_mut()
                        .kad
                        .get_record(TEST_KEY.to_vec().into());
                    get_started = true;
                    println!("get_record submitted");
                }
            }
            SwarmEvent::Behaviour(SpikeBehaviourEvent::Kad(
                kad::Event::OutboundQueryProgressed { result, .. },
            )) => match result {
                QueryResult::PutRecord(Ok(_)) => {
                    println!("PUT confirmed by the DHT");
                }
                QueryResult::PutRecord(Err(err)) => {
                    // Expected in a 2-node network with the default
                    // Quorum::One: put_record's quorum counts *other*
                    // peers that confirm storing a copy, not this node's
                    // own local store (which already has it the moment
                    // put_record is called). A real deployment's much
                    // larger routing table won't hit this — noted here
                    // since it's a real, non-obvious finding from this
                    // spike, not a bug in the round-trip below.
                    println!(
                        "PUT replication to other peers failed (expected with only one other \
                         node in the network): {err:?}"
                    );
                }
                QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(found))) => {
                    let value = String::from_utf8_lossy(&found.record.value);
                    println!("GET round-trip succeeded — value: {value:?}");
                    return Ok(());
                }
                QueryResult::GetRecord(Err(err)) => {
                    println!("GET failed: {err:?} (the listener's put may not have landed yet)");
                }
                _ => {}
            },
            SwarmEvent::Behaviour(SpikeBehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                // Feeding identify's reported listen addresses into Kademlia
                // is what actually populates the routing table for a
                // directly-dialed (not DHT-discovered) peer — without this,
                // put_record/get_record on a two-node swarm never finds a
                // provider.
                for addr in info.listen_addrs {
                    swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                }
            }
            _ => {}
        }
    }
}
