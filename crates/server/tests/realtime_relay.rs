//! Issue #539's own acceptance criterion: a presence update / chat message
//! originated on node A is observed by a subscriber connected to node B —
//! exercised against two real, separately-running `avalon-server`
//! processes sharing one Postgres, the same "second URL via env var"
//! pattern `crates/server/tests/remote_settlement.rs` already established
//! for a two-node scenario, rather than spawning child processes from
//! inside the test itself. Gated `--ignored`/live like every other
//! multi-node test in this crate.
//!
//! Setup (see this repo's two-node LAN sandbox notes, or run two local
//! processes on different ports/`AVALON_NODE_URL`s against the same
//! `DATABASE_URL`/`AVALON_NETWORK_ID`, each pointing
//! `AVALON_BOOTSTRAP_PEERS` at the other with a short
//! `AVALON_ANNOUNCE_INTERVAL_SECS` so their peer tables populate quickly):
//!
//! ```text
//! AVALON_SERVER_URL=http://<node-a>                    # this test's default target
//! AVALON_REALTIME_RELAY_PEER_SERVER_URL=http://<node-b> # the peer node to observe on
//! ```

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

/// The second, independently-running `avalon-server` process this test
/// observes relayed events on — `None` means the two-node scenario isn't
/// configured in this environment, and every test here panics with a
/// clear message rather than silently skipping (matching
/// `remote_settlement.rs`'s own convention).
fn peer_server_url() -> Option<String> {
    std::env::var("AVALON_REALTIME_RELAY_PEER_SERVER_URL").ok()
}

fn require_peer_server_url() -> String {
    peer_server_url().unwrap_or_else(|| {
        panic!(
            "AVALON_REALTIME_RELAY_PEER_SERVER_URL not set — this test needs a second, \
             independently-running avalon-server process sharing this node's DATABASE_URL/ \
             AVALON_NETWORK_ID and announcing to each other (see this file's module doc \
             comment)"
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
        .bind(format!("realtime-relay-test-{identity_id}"))
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

/// Seeds a real `indexer_identity_signing_keys` row for `identity_id` —
/// this test's identity is a direct-insert fixture (`seed_identity_session`
/// above), never having gone through the real WebAuthn
/// `/identities/register/*` ceremony `crates/server/tests/session_continuation.rs`
/// exercises, but issue #610's claim verification only ever reads this
/// table (same "authoring vs. mirror" indifference #525's own continuation
/// verification already has), so a direct insert of a freshly generated
/// keypair is exactly as good a fixture here as a real ceremony would be.
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

/// Mints a wire-encoded, self-signed `InterestClaim` (issue #610) for
/// `channel_id`, bound to `base_url` — the same thing
/// `packages/api-client/src/crypto/interestClaim.ts` mints in the browser,
/// just constructed directly against `avalon_protocol`'s own types rather
/// than round-tripping through JS.
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

/// Reads node B's `node_info` hello (issue #610) — sent once, immediately
/// after upgrade, before this test can mint a claim bound to the right
/// `base_url` (see `crate::chat::ChatServerMessage::NodeInfo`'s own doc
/// comment for why a client can't just assume its own connect URL is it).
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
                            "node B has no AVALON_NODE_URL configured — this test needs it set \
                             so a claim can be minted against it",
                        )
                        .to_string();
                }
            }
            other => panic!("expected node_info as the first message, got {other:?}"),
        }
    }
}

/// Waits until `peer_base`'s own `/nodes/peers` lists `origin_base` — the
/// announce cycle (`AVALON_ANNOUNCE_INTERVAL_SECS`) needs at least one
/// tick to run before relay has anywhere to send to. Panics after a
/// generous timeout with a message pointing at the actual setup gap,
/// rather than the relay assertions below failing confusingly for an
/// unrelated reason.
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

/// #539's presence half: publish on node A, read back on node B (self-read,
/// always visible regardless of visibility settings — see
/// `presence::presence_visible`) via the plain `GET /presence` endpoint,
/// no websocket needed for this half.
#[tokio::test]
#[ignore]
async fn a_presence_update_from_node_a_is_visible_on_node_b() {
    let http = reqwest::Client::new();
    let node_a = server_url();
    let node_b = require_peer_server_url();
    let pool = test_pool().await;

    wait_until_peered(&http, &node_b, &node_a).await;

    let (identity_id, token) = seed_identity_session(&pool).await;

    let publish = http
        .put(format!("{node_a}/me/presence"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "status": "Away" }))
        .send()
        .await
        .expect("PUT /me/presence on node A failed");
    assert!(publish.status().is_success(), "{:?}", publish.status());

    let mut observed_on_b = None;
    for _ in 0..30 {
        let response: Vec<serde_json::Value> = http
            .get(format!("{node_b}/presence?ids={identity_id}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("GET /presence on node B failed")
            .json()
            .await
            .unwrap();
        if let Some(entry) = response.first() {
            if entry["status"] == "Away" {
                observed_on_b = Some(entry.clone());
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert!(
        observed_on_b.is_some(),
        "node B never observed the presence update originated on node A — relay did not \
         reach it within the timeout"
    );
}

/// #539's chat half: create a guild on node A, subscribe to its default
/// channel over a real websocket connected to node B, send a message on
/// node A, and confirm node B's live push (not the paginated `GET`, which
/// would trivially reflect it via the shared database regardless of
/// whether relay works at all) actually delivers it — the acceptance
/// criterion's exact "observed by a subscriber connected to node B" claim.
#[tokio::test]
#[ignore]
async fn a_channel_message_from_node_a_is_pushed_to_a_subscriber_on_node_b() {
    let http = reqwest::Client::new();
    let node_a = server_url();
    let node_b = require_peer_server_url();
    let pool = test_pool().await;

    wait_until_peered(&http, &node_b, &node_a).await;

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let create_guild = http
        .post(format!("{node_a}/guilds"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": format!("Relay Test Guild {}", &suffix[..8]),
            "tag": suffix[..5].to_uppercase(),
            "description": "guild created by the #539 realtime relay test",
        }))
        .send()
        .await
        .expect("create guild on node A failed");
    assert!(
        create_guild.status().is_success(),
        "{:?}",
        create_guild.status()
    );
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap().to_string();

    let channels: Vec<serde_json::Value> = http
        .get(format!("{node_a}/guilds/{guild_id}/channels"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("list channels on node A failed")
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

    let (mut socket, _) =
        tokio_tungstenite::connect_async(ws_url(&node_b, &format!("/ws/messages?token={token}")))
            .await
            .expect("websocket connect to node B failed");

    let node_b_base_url = read_node_info_base_url(&mut socket).await;
    let claim = mint_channel_claim(
        identity_id,
        signing_key_id,
        &signing_key,
        channel_id.parse().unwrap(),
        &node_b_base_url,
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

    // Give node B's socket handler a moment to process the subscribe
    // before node A's message can possibly race ahead of it.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let send_message = http
        .post(format!(
            "{node_a}/guilds/{guild_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "body": "hello from node A, relayed to node B" }))
        .send()
        .await
        .expect("send message on node A failed");
    assert!(
        send_message.status().is_success(),
        "{:?}",
        send_message.status()
    );

    let received = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let update: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if update["type"] == "channel_message"
                        && update["data"]["body"] == "hello from node A, relayed to node B"
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
    .unwrap_or(false);

    assert!(
        received,
        "node B's websocket subscriber never received the channel message sent on node A — \
         relay did not reach it within the timeout"
    );
}
