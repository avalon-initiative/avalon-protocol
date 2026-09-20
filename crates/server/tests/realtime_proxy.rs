//! Issue #663's own acceptance criteria, live: a real Gateway process
//! (`AVALON_NODE_ROLES=gateway`, `AVALON_REALTIME_URL` pointing at the
//! second process below) and a real Realtime-only process
//! (`AVALON_NODE_ROLES=realtime`) sharing one Postgres — a WebSocket
//! client connects to the Gateway's own `/ws/presence`/`/ws/messages` and
//! genuinely receives presence/chat events that originated from another
//! identity's REST call against the *Gateway*, proving the full round
//! trip: Gateway REST handler -> `crate::realtime_relay` (#539/#584) ->
//! Realtime node's local `PresenceStore`/`ChatBus` -> the proxied
//! connection (`crate::realtime_proxy`) -> this test's client. Also
//! covers the Realtime process restarting mid-session: the client must
//! get a clean disconnect, never a silent hang.
//!
//! Gated `--ignored`/live, same manually-run-second-process convention
//! `realtime_relay.rs`/`realtime_reconnect.rs`/`internal_role_protocol.rs`
//! already establish — this test does not spawn either process itself.
//!
//! **Own isolated schema, same reason `internal_role_protocol.rs` uses
//! one**: `PostgresSettlementProvider::connect` refuses to start against a
//! database whose ledger genesis already belongs to a different
//! `network_id` (issue #173) — sharing this environment's own default
//! schema (already genesis-rooted at `avalon-dev-local`) would make every
//! run of this test fail that check for an unrelated reason. `setup_schema`
//! below creates (if needed) and migrates `test_realtime_proxy_663` via a
//! plain, unscoped `DATABASE_URL` connection — call it (or run any one test
//! in this file) once before starting either process below for the first
//! time against a fresh database, same ordering note
//! `internal_role_protocol.rs`'s own module doc comment makes.
//!
//! ```text
//! # once, before starting either process below for the first time
//! DATABASE_URL='...' cargo test -p avalon-server --test realtime_proxy \
//!   -- --ignored create_the_schema_once
//!
//! # Realtime-only process
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_realtime_proxy_663' \
//! AVALON_SERVER_ADDR=127.0.0.1:8098 AVALON_NODE_URL=http://127.0.0.1:8098 \
//! AVALON_NETWORK_ID=avalon-test-663 \
//! AVALON_NODE_ROLES=realtime AVALON_ANNOUNCE_INTERVAL_SECS=1 \
//! AVALON_BOOTSTRAP_PEERS=http://127.0.0.1:8099 \
//! AVALON_DHT_ENABLED=false \
//! cargo run -p avalon-server
//!
//! # Gateway process, proxying to the Realtime process above
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_realtime_proxy_663' \
//! AVALON_SERVER_ADDR=127.0.0.1:8099 AVALON_NODE_URL=http://127.0.0.1:8099 \
//! AVALON_NETWORK_ID=avalon-test-663 \
//! AVALON_NODE_ROLES=gateway AVALON_REALTIME_URL=http://127.0.0.1:8098 \
//! AVALON_ANNOUNCE_INTERVAL_SECS=1 AVALON_BOOTSTRAP_PEERS=http://127.0.0.1:8098 \
//! AVALON_DHT_ENABLED=false \
//! cargo run -p avalon-server
//!
//! # this test
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_realtime_proxy_663' \
//! AVALON_SERVER_URL=http://127.0.0.1:8099 \
//! AVALON_REALTIME_ROLE_SERVER_URL=http://127.0.0.1:8098 \
//! cargo test -p avalon-server --test realtime_proxy -- --ignored --test-threads=1
//!
//! # cleanup
//! # DROP SCHEMA IF EXISTS test_realtime_proxy_663 CASCADE; against DATABASE_URL
//! ```
//!
//! Both processes need the same `DATABASE_URL`/`AVALON_NETWORK_ID` (one
//! deployment's shared database, per `crate::internal_role`'s own module
//! doc comment on that trust boundary) and to announce to each other — see
//! `wait_until_peered` below, matching `realtime_reconnect.rs`'s own
//! wait-for-mesh-convergence helper.
//!
//! The last test (`realtime_process_restarting_mid_session_gives_the_client_a_clean_close`)
//! optionally kills the Realtime process itself when
//! `AVALON_REALTIME_ROLE_SERVER_PID` is set, same opt-in-kill convention
//! `internal_role_protocol.rs`'s own last test establishes — run it last,
//! since (like that one) it deliberately ends a process the earlier tests
//! in this file depend on.

use avalon_protocol::interest_claim::{signing_bytes, ClaimedScope, InterestClaim};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

const SCHEMA: &str = "test_realtime_proxy_663";

fn gateway_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8099".to_string())
}

fn realtime_role_url() -> String {
    std::env::var("AVALON_REALTIME_ROLE_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8098".to_string())
}

fn base_database_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set")
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}

/// Connects to this test's own isolated schema — see this module's own doc
/// comment for why a shared/default-schema database can't be used here.
/// Does **not** create or migrate it (that's [`setup_schema`]/
/// `create_the_schema_once` below, run once ahead of time, matching
/// `internal_role_protocol.rs`'s own convention) — the two manually-started
/// server processes' own `search_path`-scoped `DATABASE_URL` needs the
/// schema to already exist before their own boot-time migration can run.
async fn test_pool() -> PgPool {
    let base = base_database_url();
    let sep = if base.contains('?') { "&" } else { "?" };
    let scoped = format!("{base}{sep}options=-c%20search_path%3D{SCHEMA}");
    PgPoolOptions::new()
        .connect(&scoped)
        .await
        .expect("failed to connect to this test's schema — run the create_the_schema_once test first, see this module's own doc comment")
}

/// Creates (if needed) and migrates this test's own isolated schema —
/// idempotent, safe to run repeatedly. `#[ignore]`d like every other test
/// in this file (this crate's non-live `cargo test --workspace` must not
/// require `DATABASE_URL` at all), even though it has no running-server
/// dependency of its own — `CREATE SCHEMA IF NOT EXISTS` plus
/// `avalon_server::migrate::migrate_up`'s own tracking table make repeat
/// calls a no-op, so running it more than once (or alongside the other
/// tests, in any order) is harmless.
#[tokio::test]
#[ignore]
async fn create_the_schema_once() {
    let admin_pool = PgPoolOptions::new()
        .connect(&base_database_url())
        .await
        .expect("failed to connect to Postgres — is it reachable?");
    sqlx::query("CREATE SCHEMA IF NOT EXISTS test_realtime_proxy_663")
        .execute(&admin_pool)
        .await
        .expect("failed to create the test schema");
    admin_pool.close().await;

    let pool = test_pool().await;
    avalon_server::migrate::migrate_up(&pool, &migrations_dir())
        .await
        .expect("failed to migrate the test schema");
}

async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    // `presence_visibility` defaults to `friends` (issue #87/#55) — set to
    // `public` here so this test's two freshly-seeded, never-friended
    // identities can see each other's presence without also seeding a
    // friendship, which isn't otherwise relevant to what this test proves.
    sqlx::query(
        "INSERT INTO profiles (identity_id, display_name, presence_visibility) VALUES ($1, $2, 'public')",
    )
    .bind(identity_id)
    .bind(format!("realtime-proxy-test-{identity_id}"))
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

/// Same fixture `realtime_relay.rs`/`realtime_reconnect.rs` establish —
/// this identity is a direct-insert fixture, so it needs an
/// `indexer_identity_signing_keys` row for #610's claim verification to
/// ever find, independent of the real WebAuthn registration ceremony.
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

/// Waits until `peer_base`'s own `/nodes/peers` knows about `origin_base`
/// — same convergence-wait `realtime_reconnect.rs::wait_until_peered`
/// establishes, needed here so the Gateway's `relay_to_peers` call
/// (#539) actually has the Realtime node in its peer table by the time
/// this test starts asserting on delivery.
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
        tokio::time::sleep(Duration::from_millis(500)).await;
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

type WsSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
            "name": format!("Realtime Proxy Test Guild {}", &suffix[..8]),
            "tag": suffix[..5].to_uppercase(),
            "description": "guild created by the #663 realtime proxy test",
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

/// Presence: subscriber connects to the Gateway's `/ws/presence` (which,
/// with `AVALON_NODE_ROLES=gateway`+`AVALON_REALTIME_URL` set, proxies
/// through to the Realtime process per `crate::realtime_proxy`), a
/// *second* identity publishes presence via a plain `PUT /me/presence`
/// against the same Gateway, and the subscriber genuinely receives it —
/// proving the whole round trip, not just that the socket opens.
#[tokio::test]
#[ignore]
async fn presence_published_against_the_gateway_reaches_a_client_proxied_to_realtime() {
    let http = reqwest::Client::new();
    let gateway = gateway_url();
    let realtime = realtime_role_url();
    let pool = test_pool().await;

    wait_until_peered(&http, &gateway, &realtime).await;
    wait_until_peered(&http, &realtime, &gateway).await;

    let (subscriber_id, subscriber_token) = seed_identity_session(&pool).await;
    let (publisher_id, publisher_token) = seed_identity_session(&pool).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(ws_url(
        &gateway,
        &format!("/ws/presence?token={subscriber_token}"),
    ))
    .await
    .expect("websocket connect to the Gateway's /ws/presence failed");
    socket
        .send(WsMessage::text(
            serde_json::json!({ "type": "subscribe", "ids": [publisher_id] }).to_string(),
        ))
        .await
        .expect("subscribe send failed");

    // The catch-up snapshot `handle_presence_socket` sends on subscribe —
    // drained here so it isn't mistaken for the real update below.
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;

    let publish = http
        .put(format!("{gateway}/me/presence"))
        .bearer_auth(&publisher_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .expect("PUT /me/presence against the Gateway failed");
    assert!(publish.status().is_success(), "{:?}", publish.status());

    let received = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let update: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if update["identity_id"] == publisher_id.to_string()
                        && update["status"] == "Online"
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
        "a client proxied through the Gateway to the Realtime node never received a presence \
         update genuinely published against the Gateway (subscriber={subscriber_id}, \
         publisher={publisher_id}) — the Gateway -> relay -> Realtime -> proxy round trip did \
         not work"
    );
}

/// Chat: same round trip as the presence test above, for a guild channel
/// message instead — proves `crate::interest`'s DHT claim-verification
/// path (via `ChatServerMessage::NodeInfo`) still works correctly when the
/// `node_info` hello the client reads back comes from the Realtime
/// process (the node the proxied connection actually terminates on), not
/// the Gateway it dialed.
#[tokio::test]
#[ignore]
async fn a_channel_message_sent_against_the_gateway_reaches_a_client_proxied_to_realtime() {
    let http = reqwest::Client::new();
    let gateway = gateway_url();
    let realtime = realtime_role_url();
    let pool = test_pool().await;

    wait_until_peered(&http, &gateway, &realtime).await;
    wait_until_peered(&http, &realtime, &gateway).await;

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let (guild_id, channel_id) = create_guild_with_general_channel(&http, &gateway, &token).await;

    let (mut socket, _): (WsSocket, _) =
        tokio_tungstenite::connect_async(ws_url(&gateway, &format!("/ws/messages?token={token}")))
            .await
            .expect("websocket connect to the Gateway's /ws/messages failed");

    let base_url = loop {
        match socket.next().await {
            Some(Ok(WsMessage::Text(text))) => {
                let msg: serde_json::Value = serde_json::from_str(&text).unwrap();
                if msg["type"] == "node_info" {
                    break msg["data"]["base_url"]
                        .as_str()
                        .expect(
                            "the Realtime process this connection is proxied to has no \
                             AVALON_NODE_URL configured — this test needs it set so a claim can \
                             be minted against it",
                        )
                        .to_string();
                }
            }
            other => panic!("expected node_info as the first message, got {other:?}"),
        }
    };
    // The node_info base_url the client just read back must be the
    // Realtime process's own address, never the Gateway's — proof that
    // `handle_chat_socket` (and therefore its `InterestGuard`
    // registration) is genuinely running on the Realtime process, not a
    // second local copy on the Gateway.
    assert_eq!(
        base_url, realtime,
        "node_info's base_url must come from the Realtime process the connection was proxied \
         to, not the Gateway the client actually dialed"
    );

    let claim = mint_channel_claim(
        identity_id,
        signing_key_id,
        &signing_key,
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
    tokio::time::sleep(Duration::from_millis(300)).await;

    let send = http
        .post(format!(
            "{gateway}/guilds/{guild_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "body": "sent against the gateway, proxied to realtime" }))
        .send()
        .await
        .expect("send message against the Gateway failed");
    assert!(send.status().is_success(), "{:?}", send.status());

    let received = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let update: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if update["type"] == "channel_message"
                        && update["data"]["body"] == "sent against the gateway, proxied to realtime"
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
        "a client proxied through the Gateway to the Realtime node never received a channel \
         message sent against the Gateway"
    );
}

/// #663's other explicit test ask: a Realtime restart mid-session must
/// give a proxied client a clean disconnect, never a silent hang.
/// Optionally kills the real Realtime process itself when
/// `AVALON_REALTIME_ROLE_SERVER_PID` is set — same opt-in convention
/// `internal_role_protocol.rs`'s own last test uses. Run this file with
/// `--test-threads=1` (see module doc comment) so this doesn't race the
/// two tests above, which depend on the Realtime process still being up.
#[tokio::test]
#[ignore]
async fn realtime_process_restarting_mid_session_gives_the_client_a_clean_close() {
    let gateway = gateway_url();
    let pool = test_pool().await;
    let (_, token) = seed_identity_session(&pool).await;

    let (mut socket, _) =
        tokio_tungstenite::connect_async(ws_url(&gateway, &format!("/ws/presence?token={token}")))
            .await
            .expect("websocket connect to the Gateway's /ws/presence failed");
    // A live round trip first — proves the proxied connection genuinely
    // works before this test tears the far end down.
    socket
        .send(WsMessage::text(
            serde_json::json!({ "type": "subscribe", "ids": [] }).to_string(),
        ))
        .await
        .expect("subscribe send failed");

    let Ok(pid) = std::env::var("AVALON_REALTIME_ROLE_SERVER_PID") else {
        eprintln!(
            "AVALON_REALTIME_ROLE_SERVER_PID not set — skipping the actual kill. To exercise \
             it, kill the Realtime-role process manually (kill -9 <pid>) while this test's \
             socket is open, or set AVALON_REALTIME_ROLE_SERVER_PID and re-run."
        );
        return;
    };
    let status = std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .expect("failed to invoke `kill`");
    assert!(status.success(), "`kill -9 {pid}` failed");

    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), socket.next()).await;
    let elapsed = started.elapsed();

    match outcome {
        Ok(None) => {}                          // stream ended — a clean end, acceptable
        Ok(Some(Ok(WsMessage::Close(_)))) => {} // an explicit close frame — the documented behavior
        Ok(Some(Err(_))) => {} // a connection-reset error surfaced promptly — also acceptable
        other => panic!(
            "expected a clean close/end/error within 10s of the Realtime process dying, got: \
             {other:?}"
        ),
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "the proxied client did not learn the Realtime process was gone within a bounded time \
         (took {elapsed:?}) — this must be a clean disconnect, never a silent hang"
    );
}
