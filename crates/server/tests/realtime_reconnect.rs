//! Issue #541: verifies #535's decided "failover falls out for free" claim
//! against a real two-node scenario, rather than assuming it from #539's
//! design — a client connected to node A, then reconnecting to node B,
//! must resume receiving live chat events with no special session
//! hand-off logic, and any gap must be bounded to the disconnect window
//! itself, not ongoing after reconnect.
//!
//! **Scoping, stated honestly**: this test does not kill the real,
//! externally-managed node A process (this crate's live-test convention —
//! see `crates/server/tests/remote_settlement.rs`/`realtime_relay.rs` —
//! has tests observe two already-running processes via env-var URLs, not
//! control their process lifecycle). The client-side "detect my node is
//! unreachable" trigger is also separately not built yet (see
//! `NEXT_TASKS.md`'s #525 notes) — out of scope for #541, which is about
//! whether reconnecting to a *different* node actually works, not about
//! automatically detecting *when* to. What this test does exercise for
//! real: the client closes its connection to node A (standing in for any
//! reason a real client might lose its connection — a crash, a network
//! partition, a restart), opens a **fresh** connection to node B using
//! the exact same session token with zero special reconnect protocol, and
//! confirms it immediately resumes receiving live events — including more
//! than one in a row, proving the new subscription is genuinely live, not
//! a one-off coincidental delivery.
//!
//! Gated `--ignored`/live, same two-node setup
//! `crates/server/tests/realtime_relay.rs`'s module doc comment
//! documents (`AVALON_SERVER_URL` for node A,
//! `AVALON_REALTIME_RELAY_PEER_SERVER_URL` for node B).

use avalon_protocol::interest_claim::{signing_bytes, ClaimedScope, InterestClaim};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn require_peer_server_url() -> String {
    std::env::var("AVALON_REALTIME_RELAY_PEER_SERVER_URL").unwrap_or_else(|_| {
        panic!(
            "AVALON_REALTIME_RELAY_PEER_SERVER_URL not set — this test needs a second, \
             independently-running avalon-server process sharing this node's DATABASE_URL/ \
             AVALON_NETWORK_ID and announcing to each other (see realtime_relay.rs's module \
             doc comment)"
        )
    })
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("realtime-reconnect-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

/// Same fixture `crates/server/tests/realtime_relay.rs::seed_signing_key`
/// establishes — this identity is a direct-insert fixture, never having run
/// the real WebAuthn registration ceremony, but issue #610's claim
/// verification only ever reads `indexer_identity_signing_keys`.
async fn seed_signing_key(pool: &PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let signing_key_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(signing_key_id)
    .bind(identity_id)
    .bind(signing_key.verifying_key().to_bytes().to_vec())
    .execute(pool)
    .await
    .expect("failed to seed signing key");
    (signing_key_id, signing_key)
}

/// Same claim-minting helper `realtime_relay.rs::mint_channel_claim`
/// establishes.
fn mint_channel_claim(
    identity_id: Uuid,
    signing_key_id: Uuid,
    signing_key: &SigningKey,
    channel_id: Uuid,
    base_url: &str,
) -> String {
    let scope = ClaimedScope::Channel { channel_id };
    let nonce = Uuid::new_v4();
    let issued_at = time::OffsetDateTime::now_utc();
    let expires_at = issued_at + time::Duration::hours(1);
    let bytes = signing_bytes(
        identity_id,
        signing_key_id,
        scope,
        base_url,
        nonce,
        issued_at,
        expires_at,
    );
    let signature = signing_key.sign(&bytes);
    let claim = InterestClaim {
        identity_id,
        signing_key_id,
        scope,
        base_url: base_url.to_string(),
        nonce,
        issued_at,
        expires_at,
        signature: hex::encode(signature.to_bytes()),
    };
    serde_json::to_string(&claim).expect("InterestClaim always serializes")
}

async fn wait_until_peered(http: &reqwest::Client, peer_base: &str, origin_base: &str) {
    for _ in 0..60 {
        if let Ok(response) = http.get(format!("{peer_base}/nodes/peers")).send().await {
            if let Ok(peers) = response.json::<Vec<serde_json::Value>>().await {
                if peers
                    .iter()
                    .any(|p| p["base_url"].as_str() == Some(origin_base))
                {
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!(
        "{peer_base} never learned about {origin_base} via /nodes/peers — are both processes \
         announcing to each other (AVALON_BOOTSTRAP_PEERS / AVALON_NODE_URL)?"
    );
}

fn ws_url(base: &str, path: &str) -> String {
    let ws_base = base
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    format!("{ws_base}{path}")
}

async fn create_guild_with_general_channel(
    http: &reqwest::Client,
    base: &str,
    token: &str,
) -> (String, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let create_guild = http
        .post(format!("{base}/guilds"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": format!("Reconnect Test Guild {}", &suffix[..8]),
            "tag": suffix[..5].to_uppercase(),
            "description": "guild created by the #541 reconnect test",
        }))
        .send()
        .await
        .expect("create guild failed");
    assert!(
        create_guild.status().is_success(),
        "{:?}",
        create_guild.status()
    );
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap().to_string();

    let channels: Vec<serde_json::Value> = http
        .get(format!("{base}/guilds/{guild_id}/channels"))
        .bearer_auth(token)
        .send()
        .await
        .expect("list channels failed")
        .json()
        .await
        .unwrap();
    let channel_id = channels
        .iter()
        .find(|c| c["name"] == "general")
        .expect("every guild gets a default general channel")["id"]
        .as_str()
        .unwrap()
        .to_string();

    (guild_id, channel_id)
}

async fn read_node_info_base_url(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> String {
    loop {
        match socket.next().await {
            Some(Ok(WsMessage::Text(text))) => {
                let msg: serde_json::Value = serde_json::from_str(&text).unwrap();
                if msg["type"] == "node_info" {
                    return msg["data"]["base_url"]
                        .as_str()
                        .expect(
                            "this node has no AVALON_NODE_URL configured — this test needs it \
                             set so a claim can be minted against it",
                        )
                        .to_string();
                }
            }
            other => panic!("expected node_info as the first message, got {other:?}"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn subscribe_channel_socket(
    base: &str,
    token: &str,
    guild_id: &str,
    channel_id: &str,
    identity_id: Uuid,
    signing_key_id: Uuid,
    signing_key: &SigningKey,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (mut socket, _) =
        tokio_tungstenite::connect_async(ws_url(base, &format!("/ws/messages?token={token}")))
            .await
            .expect("websocket connect failed");
    let base_url = read_node_info_base_url(&mut socket).await;
    let claim = mint_channel_claim(
        identity_id,
        signing_key_id,
        signing_key,
        channel_id.parse().unwrap(),
        &base_url,
    );
    socket
        .send(WsMessage::text(
            serde_json::json!({
                "type": "subscribe_channel",
                "guild_id": guild_id,
                "channel_id": channel_id,
                "claim": claim,
            })
            .to_string(),
        ))
        .await
        .expect("subscribe_channel send failed");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    socket
}

async fn expect_channel_message_body(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    expected_body: &str,
) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let update: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if update["type"] == "channel_message"
                        && update["data"]["body"] == expected_body
                    {
                        return true;
                    }
                }
                Some(Ok(_)) => continue,
                _ => return false,
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// #541's core scenario: connect to node A, confirm live delivery there,
/// disconnect, reconnect to node B with the same token and zero special
/// protocol, and confirm delivery resumes — for more than one message in
/// a row, so the gap is provably bounded to the disconnect window itself
/// rather than the new connection being a one-off fluke.
#[tokio::test]
#[ignore]
async fn reconnecting_to_a_different_node_resumes_live_delivery_with_no_special_handoff() {
    let http = reqwest::Client::new();
    let node_a = server_url();
    let node_b = require_peer_server_url();
    let pool = test_pool().await;

    wait_until_peered(&http, &node_b, &node_a).await;
    wait_until_peered(&http, &node_a, &node_b).await;

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let (guild_id, channel_id) = create_guild_with_general_channel(&http, &node_a, &token).await;

    // Connected to node A — a message sent (anywhere) is delivered here,
    // the same baseline #539 already proves.
    let mut socket_a = subscribe_channel_socket(
        &node_a,
        &token,
        &guild_id,
        &channel_id,
        identity_id,
        signing_key_id,
        &signing_key,
    )
    .await;
    let send_while_on_a = http
        .post(format!(
            "{node_a}/guilds/{guild_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "body": "message while connected to node A" }))
        .send()
        .await
        .expect("send while on A failed");
    assert!(send_while_on_a.status().is_success());
    assert!(
        expect_channel_message_body(&mut socket_a, "message while connected to node A").await,
        "sanity check failed: node A's own subscriber never received a message sent on node A"
    );

    // The client loses its connection to node A (crash, restart, network
    // partition — the specific cause doesn't matter to this test) and
    // reconnects to node B instead, with nothing beyond its ordinary
    // session token — no continuation ceremony, no server-side state
    // carried over.
    socket_a
        .close(None)
        .await
        .expect("closing node A's socket failed");
    let mut socket_b = subscribe_channel_socket(
        &node_b,
        &token,
        &guild_id,
        &channel_id,
        identity_id,
        signing_key_id,
        &signing_key,
    )
    .await;

    // First message after reconnecting — proves delivery resumed at all.
    let send_after_reconnect_1 = http
        .post(format!(
            "{node_a}/guilds/{guild_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "body": "first message after reconnect" }))
        .send()
        .await
        .expect("send after reconnect (1) failed");
    assert!(send_after_reconnect_1.status().is_success());
    assert!(
        expect_channel_message_body(&mut socket_b, "first message after reconnect").await,
        "node B's subscriber never received the first message sent after reconnecting — \
         failover did not actually resume live delivery"
    );

    // Second message — proves the gap really was bounded to the
    // disconnect window, not an ongoing loss that happened to let exactly
    // one message through.
    let send_after_reconnect_2 = http
        .post(format!(
            "{node_a}/guilds/{guild_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "body": "second message after reconnect" }))
        .send()
        .await
        .expect("send after reconnect (2) failed");
    assert!(send_after_reconnect_2.status().is_success());
    assert!(
        expect_channel_message_body(&mut socket_b, "second message after reconnect").await,
        "node B's subscriber received the first post-reconnect message but not the second — \
         the gap is not bounded to the disconnect window, delivery stopped again"
    );
}
