//! Issue #540: async at-rest replication of guild chat history to at
//! least one additional node (`crate::chat_replication`), verified
//! against two real, separately-running `avalon-server` processes, same
//! two-node convention `crates/server/tests/realtime_relay.rs` already
//! established (`AVALON_SERVER_URL` for node A, which sends the message;
//! `AVALON_REALTIME_RELAY_PEER_SERVER_URL` for node B, the replication
//! target — both must advertise `AVALON_NODE_ROLES` that make node B
//! `indexer`/`combined`-eligible, matching `chat_replication`'s own
//! target-selection rule).
//!
//! **Scoping, stated honestly.** This environment's `avalon` Postgres
//! role has no `CREATEDB` privilege (confirmed: `sqlx database create`
//! against a second database name returns "permission denied to create
//! database"), so a genuinely separate second Postgres — the setup that
//! would let this test prove the full disaster-recovery claim ("kill node
//! A's *database*, read the message back from node B's own, physically
//! separate copy") — isn't available here, the same real limitation
//! `crates/server/tests/remote_settlement.rs`'s module doc comment
//! already documents for its own second-database scenario. What this
//! test verifies for real instead: the replication mechanism itself
//! actually runs end to end over the real network path — node A's
//! `send_message` triggers a real `POST /nodes/replicate-chat` to node
//! B, which node B's own handler decodes and writes into its own
//! `guild_messages_replica` table (a table nothing else in this codebase
//! ever writes to, so a correctly-populated row is real proof the code
//! path executed, not an artifact of the two processes happening to share
//! a database in this environment) — and that a delete event correctly
//! marks the replica row `deleted_at`, not silently ignored.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn require_peer_server_url() -> String {
    std::env::var("AVALON_REALTIME_RELAY_PEER_SERVER_URL").unwrap_or_else(|_| {
        panic!(
            "AVALON_REALTIME_RELAY_PEER_SERVER_URL not set — this test needs a second, \
             independently-running avalon-server process, reachable from this one, \
             advertising an indexer/combined AVALON_NODE_ROLES (see this file's module doc \
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
        .bind(format!("chat-replication-test-{identity_id}"))
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

/// #540's core scenario: send a message via node A, confirm it lands
/// (async, so poll) in node B's `guild_messages_replica`, then delete it
/// and confirm the replica row is marked `deleted_at` rather than either
/// staying untouched or being physically removed.
#[tokio::test]
#[ignore]
async fn a_channel_message_from_node_a_is_replicated_to_node_bs_replica_table() {
    let http = reqwest::Client::new();
    let node_a = server_url();
    let node_b = require_peer_server_url();
    let node_a_pool = test_pool().await;

    wait_until_peered(&http, &node_b, &node_a).await;

    let (_identity_id, token) = seed_identity_session(&node_a_pool).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let create_guild = http
        .post(format!("{node_a}/guilds"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": format!("Chat Replication Test Guild {}", &suffix[..8]),
            "tag": suffix[..5].to_uppercase(),
            "description": "guild created by the #540 chat replication test",
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
        .get(format!("{node_a}/guilds/{guild_id}/channels"))
        .bearer_auth(&token)
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

    let send = http
        .post(format!(
            "{node_a}/guilds/{guild_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "body": "replicate me to node B" }))
        .send()
        .await
        .expect("send message failed");
    assert!(send.status().is_success(), "{:?}", send.status());
    let message: serde_json::Value = send.json().await.unwrap();
    let message_id: Uuid = message["id"].as_str().unwrap().parse().unwrap();

    // Async replication — poll node B's own database for the replica row
    // (not node A's — same-DB test environments would trivially "pass"
    // this by coincidence if checked against node A's own pool instead;
    // reading node B's DATABASE_URL specifically is what actually proves
    // the network round trip happened).
    let node_b_database_url = std::env::var("AVALON_REALTIME_RELAY_PEER_DATABASE_URL")
        .unwrap_or_else(|_| std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"));
    let node_b_pool = PgPoolOptions::new()
        .connect(&node_b_database_url)
        .await
        .expect("failed to connect to node B's Postgres");

    let mut replicated_row: Option<sqlx::postgres::PgRow> = None;
    for _ in 0..30 {
        replicated_row =
            sqlx::query("SELECT body, deleted_at FROM guild_messages_replica WHERE id = $1")
                .bind(message_id)
                .fetch_optional(&node_b_pool)
                .await
                .expect("query failed");
        if replicated_row.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    let row = replicated_row
        .expect("message was never replicated into node B's guild_messages_replica table");
    let body: String = row.try_get("body").unwrap();
    assert_eq!(body, "replicate me to node B");
    let deleted_at: Option<time::OffsetDateTime> = row.try_get("deleted_at").unwrap();
    assert!(deleted_at.is_none(), "replica row should not start deleted");

    let delete = http
        .delete(format!(
            "{node_a}/guilds/{guild_id}/channels/{channel_id}/messages/{message_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete message failed");
    assert!(delete.status().is_success(), "{:?}", delete.status());

    let mut marked_deleted = false;
    for _ in 0..30 {
        let row = sqlx::query("SELECT deleted_at FROM guild_messages_replica WHERE id = $1")
            .bind(message_id)
            .fetch_one(&node_b_pool)
            .await
            .expect("query failed");
        let deleted_at: Option<time::OffsetDateTime> = row.try_get("deleted_at").unwrap();
        if deleted_at.is_some() {
            marked_deleted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert!(
        marked_deleted,
        "the deletion was never replicated — node B's replica row still shows deleted_at NULL"
    );
}
