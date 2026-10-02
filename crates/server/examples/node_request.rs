//! Calls the node-to-node write routes of one node as a throwaway libp2p identity that never
//! announced, over a libp2p stream and over signed HTTP, for the NAT scenario suite
//! (`scripts/nat-scenarios.sh`).
//!
//! usage: node_request <network id> <listen multiaddr> <target peer id> <target multiaddr> <target http url>
//!
//! Calls each route first as a stranger (`free`), then announces as `p2p://<its peer id>` over the
//! stream and calls them again (`standing`). Prints one `<phase> <transport> <path> <status> <code>`
//! line per call; `code` is the refusal code in the response body, or `-` when there is none.

use std::time::Duration;

use avalon_server::dht::{self, DhtConfig};
use avalon_server::node_auth::CREDENTIAL_PATHS;
use avalon_server::node_http::{
    p2p_base_url, NodeClient, NodeHttpSettings, NodeSigner, StreamHandle,
};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::AutonatSettings;
use avalon_server::relay::RelaySettings;
use libp2p::{identity, PeerId};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [network, listen, target_id, target_addr, target_url] = args.as_slice() else {
        anyhow::bail!(
            "usage: node_request <network id> <listen multiaddr> <target peer id> <target multiaddr> <target http url>"
        );
    };
    let target: PeerId = target_id.parse()?;
    let key = identity::Keypair::generate_ed25519();
    let peers = PeerTable::new();
    peers.upsert(PeerInfo {
        base_url: target_url.trim_end_matches('/').to_string(),
        roles: vec!["combined".into()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: network.clone(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(target.to_string()),
        libp2p_listen_addrs: vec![target_addr.clone()],
        witness: None,
        connectivity: None,
        identity_bound: true,
    });
    let handle = dht::start_with_node_http(
        peers.clone(),
        DhtConfig {
            identity: key.clone(),
            network_id: network.clone(),
            listen_addr: listen.parse()?,
            external_addr: None,
            autonat: AutonatSettings::default(),
            relay: RelaySettings::default(),
            relay_resilience: Default::default(),
            bootstrap_scan_interval: Duration::from_secs(1),
        },
        NodeHttpSettings::default(),
    )
    .await;
    let stream = StreamHandle {
        commands: handle.commands.clone(),
        peers: Some(peers),
        settings: NodeHttpSettings::default(),
    };
    let signer = NodeSigner::new(&key, network).ok_or_else(|| anyhow::anyhow!("no signer"))?;
    println!("caller {}", handle.peer_id);

    let stream_client = NodeClient::new().with_stream(stream.clone());
    let signed_http = NodeClient::new()
        .with_stream(stream.clone())
        .with_signer(signer);
    let unsigned_http = NodeClient::new().with_stream(stream);
    let own = handle.peer_id.to_string();
    let announce = serde_json::json!({
        "base_url": format!("p2p://{own}"),
        "libp2p_peer_id": own,
        "roles": ["combined"],
        "protocol_version": avalon_server::version::PROTOCOL_VERSION,
        "network_id": network,
        "coordinate": {"vector": [0, 0, 0], "height": 0.01, "error": 1.0},
    });
    let message = serde_json::json!({"type": "channel_message", "data": {
        "id": uuid::Uuid::new_v4(), "channel_id": uuid::Uuid::new_v4(),
        "author": avalon_protocol::identity_id::TestIdentity::new().id, "body": "credential lab",
        "sent_at": time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,
    }});
    let notify = serde_json::json!({"network_id": network, "tree_size": 1});
    for phase in ["free", "standing"] {
        if phase == "standing" {
            let (status, _) = call(
                &stream_client,
                format!("{}/nodes/announce", p2p_base_url(&target)),
                &announce,
            )
            .await;
            println!("announce stream /nodes/announce {status} -");
            // The announcer is listed once the neighbor has stored it.
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        for path in CREDENTIAL_PATHS {
            let body = match *path {
                "/nodes/replicate-chat" => &message,
                "/mirror/notify" => &notify,
                _ => &serde_json::json!({}),
            };
            let calls = [
                ("stream", &stream_client, p2p_base_url(&target)),
                ("http-signed", &signed_http, target_url.clone()),
                ("http-unsigned", &unsigned_http, target_url.clone()),
            ];
            for (transport, client, base) in calls {
                let url = format!("{}{path}", base.trim_end_matches('/'));
                let (status, code) = call(client, url, body).await;
                println!("{phase} {transport} {path} {status} {code}");
            }
        }
    }
    Ok(())
}

/// POSTs `body`, retrying while the first stream request races the dial to the target.
async fn call(client: &NodeClient, url: String, body: &serde_json::Value) -> (String, String) {
    for _ in 0..10 {
        match client.post(&url).json(body).send().await {
            Err(e) if e.is_connect() => tokio::time::sleep(Duration::from_secs(1)).await,
            Err(e) => return ("error".into(), e.to_string().replace(' ', "_")),
            Ok(response) => {
                let status = response.status().as_u16().to_string();
                let text = response.text().await.unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["code"].as_str().map(str::to_string))
                    .unwrap_or_else(|| "-".into());
                return (status, code);
            }
        }
    }
    ("error".into(), "no_connection".into())
}
