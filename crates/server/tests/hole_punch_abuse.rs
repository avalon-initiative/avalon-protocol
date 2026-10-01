//! Hostile peers on the DCUtR hole-punch exchange, between real in-process swarms over
//! loopback: a peer connected through a relay answers the hole-punch handshake with garbage,
//! truncated and oversized frames, or opens hostile hole-punch streams itself. The reserved
//! node must record a failed punch, never a punched peer, and stay reachable. No database
//! needed.
//!
//! The frame decoding is libp2p's; what these tests pin is that Avalon's worker survives it,
//! reports the failure and keeps the relayed path working, so they are named `libp2p_*`.

mod abuse_support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use abuse_support::*;
use avalon_protocol::connectivity::Connectivity;
use avalon_server::dht::DhtHandle;
use avalon_server::reachability::connectivity_for;
use avalon_server::relay::RelayServerSettings;
use libp2p::futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{dcutr, Multiaddr, PeerId, Swarm};

/// `hostile` dials `relayed` and waits until it is connected to `target` over the circuit.
async fn connect_via_circuit(hostile: &mut Swarm<Hostiles>, relayed: Multiaddr, target: PeerId) {
    hostile.dial(relayed).unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } = hostile.select_next_some().await
            {
                if peer_id == target && endpoint.is_relayed() {
                    return;
                }
            }
        }
    })
    .await
    .expect("the hostile peer reaches the node through the relay");
}

/// The node still holds its reservation, has punched nobody and an honest peer reaches it
/// through the relay.
async fn assert_still_relayed_and_reachable(a: &DhtHandle, relayed: Multiaddr) {
    let snap = a.reachability.snapshot();
    assert_eq!(snap.relay_reservations.len(), 1);
    assert!(snap.punched_peers.is_empty(), "{:?}", snap.punched_peers);
    assert_eq!(connectivity_for(&snap), Some(Connectivity::Relayed));
    let mut honest = raw_swarm();
    dial_through_relay(&mut honest, relayed, a.peer_id)
        .await
        .expect("an honest peer is still served through the relay");
}

/// A hole-punch handshake answered with garbage, nothing, or an oversized frame fails cleanly:
/// the failure is recorded against the peer, no direct connection is claimed, and the relayed
/// path keeps working.
#[tokio::test]
async fn libp2p_a_hostile_dcutr_answer_is_recorded_as_a_failed_punch() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings::default()).await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;

    let replies: [(&str, Vec<u8>); 3] = [
        ("garbage", vec![0xff; 64]),
        ("empty", Vec::new()),
        (
            "oversized length",
            [vec![0xff, 0xff, 0xff, 0xff, 0x0f], vec![1; 32]].concat(),
        ),
    ];
    for (name, reply) in replies {
        let (mut hostile, _) = hostile_swarm(dcutr::PROTOCOL_NAME);
        let hostile_id = *hostile.local_peer_id();
        connect_via_circuit(&mut hostile, relayed.clone(), a.peer_id).await;
        let asked = answer_inbound(&mut hostile, &[reply], Duration::from_secs(10)).await;
        assert_eq!(
            asked.len(),
            1,
            "{name}: the node did not start a hole punch"
        );
        // The first message of the node's CONNECT is a length-delimited frame.
        assert!(!asked[0].is_empty(), "{name}");

        let outcome = wait_snapshot(&a, "a recorded hole punch", |s| {
            s.hole_punches
                .iter()
                .find(|h| h.peer_id == hostile_id.to_string())
                .cloned()
        })
        .await;
        assert!(!outcome.succeeded, "{name}");
        assert!(outcome.error.is_some(), "{name}");
        assert_still_relayed_and_reachable(&a, relayed.clone()).await;
    }
}

/// A peer on a relayed connection that opens hole-punch streams of its own with malformed,
/// truncated or oversized frames, including a rapid burst, gets nothing: no punched peer, no
/// panic, and the node stays reachable.
#[tokio::test]
async fn libp2p_hostile_dcutr_streams_from_a_relayed_peer_are_refused() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings::default()).await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;

    let (mut hostile, written) = hostile_swarm(dcutr::PROTOCOL_NAME);
    connect_via_circuit(&mut hostile, relayed.clone(), a.peer_id).await;

    let cases: Vec<(&str, Hostile, usize)> = vec![
        ("garbage bytes", Hostile::bytes(&[0xff; 64]), usize::MAX),
        ("empty stream", Hostile::bytes(&[]), usize::MAX),
        (
            "length prefix with no body",
            Hostile::bytes(&[100]),
            usize::MAX,
        ),
        (
            "invalid protobuf of the declared length",
            Hostile::bytes(&[vec![8], vec![0xff; 8]].concat()),
            usize::MAX,
        ),
        (
            "length beyond any limit then a flood",
            Hostile::flooding(&[0x80, 0x80, 0x80, 0x80, 0x04], Duration::from_secs(2)),
            1 << 20,
        ),
    ];
    for (name, request, max_written) in cases {
        let before = written.load(Ordering::SeqCst);
        let outcome = send_all(&mut hostile, a.peer_id, vec![request]).await;
        assert!(
            match &outcome[0] {
                Outcome::Answered(b) => b.is_empty(),
                Outcome::Failed(_) => true,
            },
            "{name}: {:?}",
            outcome[0]
        );
        assert!(
            written.load(Ordering::SeqCst) - before < max_written,
            "{name}: the node kept reading a refused frame"
        );
    }
    let rapid: Vec<Hostile> = (0..100).map(|_| Hostile::bytes(&[0xff; 16])).collect();
    assert_eq!(send_all(&mut hostile, a.peer_id, rapid).await.len(), 100);

    assert_still_relayed_and_reachable(&a, relayed).await;
}
