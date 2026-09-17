//! Disaster-recovery proof for issue #43: "every database in the network
//! disappears; what comes back?" Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Drives real identities through the actual WebAuthn ceremony (same
//! approach `crates/server/tests/passkeys.rs` already takes) rather than
//! seeding `identities`/`profiles` directly via SQL, the shortcut most
//! other feature tests in this directory use — that shortcut would make
//! `profiles` diverge from `ledger_entries` on purpose (a profile row with
//! no `identity.created` event behind it), which is exactly the gap this
//! test exists to catch, not paper over.
//!
//! **Scope note, given the repo's actual current wiring**: `profiles`,
//! `indexer_friendships`, and `indexer_guild_members` are all kept live by
//! request handlers calling `PostgresIndexer::apply_in_tx` today
//! (`identity.created`/`profile.updated` via `handlers::register_finish`/
//! `update_profile`; `friend.*` via `friends.rs`; `guild.*` via
//! `guilds.rs`, issue #506). So:
//!
//! - For `profiles`, this test compares real pre-rebuild state against
//!   post-rebuild state directly — the strong "nothing was lost" claim,
//!   since `profiles` genuinely is continuously projected today.
//! - For `indexer_friendships`/`indexer_guild_members`, this test instead
//!   asserts the rebuilt rows exactly match what the real friend/guild
//!   actions should produce, plus that a second rebuild reproduces the
//!   identical snapshot (idempotency) — kept as its own explicit assertion
//!   rather than folded into the `profiles` byte-for-byte comparison,
//!   since it's still useful as a readable, self-contained expectation.

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PROJECTION_TABLES;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::time::Duration;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

type VirtualClient =
    Client<MemoryStore, MockUserValidationMethod, public_suffix::PublicSuffixList, ()>;

fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(2),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Registers a brand-new identity entirely via the real HTTP ceremony (same
/// approach `crates/server/tests/passkeys.rs::create_identity_with_one_passkey`
/// takes), then logs it in. Returns the identity id and its session token.
async fn register_and_login(http: &reqwest::Client, base: &str) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    let display_name = format!("rebuild-test-{identity_id}");

    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "display_name": display_name }))
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client = new_virtual_client();
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let signing_bytes =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes);

    http.post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": credential,
            "event_signing_public_key": BASE64.encode(signing_key.verifying_key().to_bytes()),
            "event_signature": BASE64.encode(signature.to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("register/finish should succeed");

    let start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(Origin::from(&origin), request_options, DefaultClientData)
        .await
        .expect("virtual authenticator authentication should succeed");

    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&serde_json::json!({ "ticket_id": ticket_id, "credential": assertion }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("sessions/finish should succeed")
        .json()
        .await
        .unwrap();

    (identity_id, finish["token"].as_str().unwrap().to_string())
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

/// Polls `protocol_outbox` until every enqueued event has been drained into
/// `ledger_entries` — `avalon-server`'s outbox worker runs on its own timer
/// (`AVALON_OUTBOX_POLL_INTERVAL_SECS`, issue #363), so this test can't just
/// act and immediately read the ledger.
async fn wait_for_outbox_drain(pool: &PgPool) {
    for _ in 0..100 {
        let status = avalon_server::outbox::status(pool)
            .await
            .expect("failed to read outbox status");
        if status.pending_count == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!(
        "outbox did not drain within the timeout — is the outbox worker running (`make start`)?"
    );
}

/// One row per table, each rendered as a JSON object and sorted, so two
/// snapshots compare equal iff their actual row contents are identical —
/// not just row counts. `row_to_json` preserves the table's column order,
/// which stays stable across both snapshots since no migration runs
/// between them.
///
/// `indexer_applied_events.applied_at` is excluded from the projected
/// columns: it's a dedup bookkeeping timestamp (`DEFAULT now()`), not
/// projected data — it is expected, correctly, to differ between two
/// otherwise-identical rebuild passes, and comparing it would make the
/// idempotency assertion fail for a reason that has nothing to do with
/// whether the rebuild is actually deterministic.
async fn snapshot_table(pool: &PgPool, table: &str) -> Vec<String> {
    let query = if table == "indexer_applied_events" {
        "SELECT row_to_json(t)::text AS row FROM (SELECT event_id FROM indexer_applied_events) t \
         ORDER BY row_to_json(t)::text"
            .to_string()
    } else {
        format!("SELECT row_to_json(t)::text AS row FROM {table} t ORDER BY row_to_json(t)::text")
    };
    sqlx::query(sqlx::AssertSqlSafe(query))
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| panic!("failed to snapshot {table}: {e}"))
        .into_iter()
        .map(|row| row.try_get::<String, _>("row").unwrap())
        .collect()
}

async fn snapshot_all_projections(pool: &PgPool) -> Vec<(String, Vec<String>)> {
    let mut snapshot = Vec::with_capacity(PROJECTION_TABLES.len());
    for table in PROJECTION_TABLES {
        snapshot.push((table.to_string(), snapshot_table(pool, table).await));
    }
    snapshot
}

async fn chain(pool: &PgPool) -> PostgresSettlementProvider {
    let network_id = PostgresSettlementProvider::read_genesis_network_id(pool)
        .await
        .expect("failed to read genesis")
        .expect("genesis must already be set on a running node");
    PostgresSettlementProvider::new(pool.clone(), network_id)
}

// --- Integrator Space helpers, issue #533's own fixture ---------------------
//
// Slimmed copies of `crates/server/tests/integrator_data.rs`'s own helpers —
// this file is otherwise identity/friend/guild-scoped and has no existing
// integrator registration/auth fixture to reuse.

struct RebuildTestIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RebuildTestIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("rebuild-533-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Rebuild Test Integrator {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": [],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    RebuildTestIntegrator {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn integrator_auth_headers(
    http: &reqwest::Client,
    base: &str,
    integrator: &RebuildTestIntegrator,
) -> reqwest::header::HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.signing_key.sign(&nonce);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-avalon-integrator-key-id",
        integrator.key_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-challenge-id",
        challenge_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-signature",
        BASE64.encode(signature.to_bytes()).parse().unwrap(),
    );
    headers
}

const REBUILD_TEST_CHARACTER_PROTO: &str =
    "syntax = \"proto3\"; message Character { uint32 level = 1; }";

/// Issue #533: publish an Integrator Space instance (a "character"), delete
/// it via the new tombstone endpoint, then rebuild the entire index from
/// `ledger_entries` alone and confirm the projection comes back
/// byte-for-byte identical — the original `game_data.published` event and
/// the `game_data.deleted` tombstone are both real, durable ledger
/// entries, so replaying them must reproduce the exact same "deleted"
/// projection state, not just leave the deletion looking coincidentally
/// correct because it was never actually dropped from memory.
#[tokio::test]
#[ignore]
async fn rebuild_reproduces_integrator_data_deletion() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let integrator = register_integrator(&http, &base).await;
    let (subject_id, subject_token) = register_and_login(&http, &base).await;

    let connect = auth(
        http.post(format!("{base}/integrations/{}/connect", integrator.slug)),
        &subject_token,
    )
    .json(&serde_json::json!({ "capabilities": [] }))
    .send()
    .await
    .expect("connect failed");
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let schema_headers = integrator_auth_headers(&http, &base, &integrator).await;
    let published_schema: serde_json::Value = http
        .post(format!("{base}/integrations/{}/schemas", integrator.slug))
        .headers(schema_headers)
        .json(&serde_json::json!({ "proto_source": REBUILD_TEST_CHARACTER_PROTO }))
        .send()
        .await
        .expect("publish schema failed")
        .json()
        .await
        .unwrap();
    let version = published_schema["version"].as_u64().unwrap();

    let publish_headers = integrator_auth_headers(&http, &base, &integrator).await;
    let published_instance: serde_json::Value = http
        .post(format!(
            "{base}/integrations/{}/schemas/{version}/data",
            integrator.slug
        ))
        .headers(publish_headers)
        .json(&serde_json::json!({
            "subject": subject_id,
            "instance": { "level": 42 },
        }))
        .send()
        .await
        .expect("publish instance failed")
        .json()
        .await
        .unwrap();
    let instance_id = published_instance["id"].as_str().unwrap();

    wait_for_outbox_drain(&pool).await;

    // Confirmed live before deletion — same "the character exists" read
    // path the Hub uses.
    let before_delete: serde_json::Value = http
        .get(format!("{base}/identities/{subject_id}/integrator-data"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        before_delete
            .as_array()
            .unwrap()
            .iter()
            .any(|inst| inst["schema"] == published_schema["id"]),
        "expected the published instance to be visible before deletion"
    );

    let delete_headers = integrator_auth_headers(&http, &base, &integrator).await;
    let delete_response = http
        .delete(format!(
            "{base}/integrations/{}/schemas/{version}/data/{subject_id}",
            integrator.slug
        ))
        .headers(delete_headers)
        .json(
            &serde_json::json!({ "reason_code": "deleted", "reason": "player deleted character" }),
        )
        .send()
        .await
        .expect("delete instance failed");
    assert!(
        delete_response.status().is_success(),
        "{:?}",
        delete_response.status()
    );

    wait_for_outbox_drain(&pool).await;

    // The tombstone event is real, durable ledger history — never a
    // physical delete/mutation of the original entry, per
    // docs/architecture/revocation.md's own standard.
    let ledger_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM ledger_entries WHERE payload->>'id' = $1 OR payload->>'instance_id' = $1 ORDER BY seq",
    )
    .bind(instance_id)
    .fetch_all(&pool)
    .await
    .expect("failed to read ledger_entries");
    assert_eq!(
        ledger_kinds,
        vec![
            "game_data.published".to_string(),
            "game_data.deleted".to_string()
        ],
        "expected exactly the publish then the tombstone, both durable"
    );

    let after_delete: serde_json::Value = http
        .get(format!("{base}/identities/{subject_id}/integrator-data"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        after_delete
            .as_array()
            .unwrap()
            .iter()
            .all(|inst| inst["schema"] != published_schema["id"]),
        "expected the deleted instance to no longer be visible"
    );

    // The real, continuously-projected state before anything is dropped —
    // `indexer_integrator_data_instances` is kept live by
    // `integrator_data::publish_instance`/`delete_instance` calling
    // `apply_in_tx` directly, same posture this file's `profiles` check
    // already relies on.
    let projection_before = snapshot_table(&pool, "indexer_integrator_data_instances").await;
    assert!(
        projection_before
            .iter()
            .any(|row| row.contains(instance_id)
                && row.contains("\"delete_reason_code\":\"deleted\"")),
        "expected the tombstoned row to already carry its deletion columns before rebuild"
    );

    let chain = chain(&pool).await;
    avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool)
        .await
        .expect("rebuild failed");

    let projection_after = snapshot_table(&pool, "indexer_integrator_data_instances").await;
    assert_eq!(
        projection_before, projection_after,
        "integrator data instance projection (including its #533 tombstone columns) must come back byte-for-byte identical after a full rebuild"
    );
}

/// The core disaster-recovery proof: register two identities (real
/// `identity.created` events), update one's profile, friend-request +
/// accept between them, and have both join a guild — then rebuild the
/// entire index from `ledger_entries` alone and check what comes back.
#[tokio::test]
#[ignore]
async fn rebuild_reproduces_projections_exactly() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (alice_id, alice_token) = register_and_login(&http, &base).await;
    let (bob_id, bob_token) = register_and_login(&http, &base).await;

    let update = auth(http.patch(format!("{base}/me")), &alice_token)
        .json(&serde_json::json!({ "bio": "rebuild-from-events test fixture" }))
        .send()
        .await
        .expect("PATCH /me failed");
    assert!(update.status().is_success(), "{:?}", update.status());

    let create_request = auth(http.post(format!("{base}/friends/requests")), &alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .expect("create friend request failed");
    assert!(create_request.status().is_success());
    let request_body: serde_json::Value = create_request.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();

    let accept = auth(
        http.post(format!("{base}/friends/requests/{request_id}/accept")),
        &bob_token,
    )
    .send()
    .await
    .expect("accept friend request failed");
    assert!(accept.status().is_success());

    let suffix = Uuid::new_v4().simple().to_string();
    let create_guild = auth(http.post(format!("{base}/guilds")), &alice_token)
        .json(&serde_json::json!({
            "name": format!("Rebuild Test Guild {}", &suffix[..8]),
            "tag": suffix[..5].to_uppercase(),
            "description": "a guild created by the #43 rebuild test",
        }))
        .send()
        .await
        .expect("create guild failed");
    assert!(
        create_guild.status().is_success(),
        "{:?}",
        create_guild.status()
    );
    let guild_body: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild_body["id"].as_str().unwrap().to_string();

    // Guild creation always starts `invite_only` (no body override) — open
    // it up so bob can `POST .../join` directly rather than needing a
    // separate invite/accept round trip this test doesn't otherwise care
    // about.
    let open_policy = auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &alice_token,
    )
    .json(&serde_json::json!({ "join_policy": "open" }))
    .send()
    .await
    .expect("update guild join_policy failed");
    assert!(
        open_policy.status().is_success(),
        "{:?}",
        open_policy.status()
    );

    let join = auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &bob_token,
    )
    .send()
    .await
    .expect("join guild failed");
    assert!(join.status().is_success(), "{:?}", join.status());

    wait_for_outbox_drain(&pool).await;

    // The real, continuously-projected state before anything is dropped —
    // `profiles` is the one table request handlers already keep live via
    // the indexer (see this file's module doc).
    let profiles_before = snapshot_table(&pool, "profiles").await;
    assert!(
        profiles_before
            .iter()
            .any(|row| row.contains(&alice_id.to_string())
                && row.contains("rebuild-from-events test fixture")),
        "expected alice's updated profile to be present before rebuild"
    );

    let started = std::time::Instant::now();
    let chain = chain(&pool).await;
    let report = avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool)
        .await
        .expect("rebuild failed");
    let elapsed = started.elapsed();
    println!(
        "rebuild: {} ledger entries -> {} events applied ({} skipped) in {:.2?}",
        report.entries_read, report.events_applied, report.entries_skipped_undecodable, elapsed
    );
    // `entries_skipped_undecodable` is not asserted to be zero here: this
    // test runs against the shared dev ledger (`list_entries` reads the
    // whole thing, not just this fixture's own entries), and other live
    // tests (e.g. retention pruning) legitimately produce entries with a
    // pruned payload, which `to_protocol_event` correctly can't decode.
    // The fixture-scoped assertions below are what this test actually
    // proves.

    let profiles_after = snapshot_table(&pool, "profiles").await;
    assert_eq!(
        profiles_before, profiles_after,
        "profiles projection must come back byte-for-byte identical after a full rebuild"
    );

    // `indexer_friendships`/`indexer_guild_members` have no live writer to
    // diff against before #44 — assert the rebuilt rows directly instead
    // (see this file's module doc).
    let friendship_rows: Vec<(Uuid, Uuid)> =
        sqlx::query("SELECT a, b FROM indexer_friendships WHERE a = $1 OR a = $2")
            .bind(alice_id.min(bob_id))
            .bind(alice_id.max(bob_id))
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("a"), row.get("b")))
            .collect();
    assert_eq!(
        friendship_rows,
        vec![(alice_id.min(bob_id), alice_id.max(bob_id))],
        "rebuilt indexer_friendships must contain exactly the accepted pair"
    );

    // `[alice, bob]`, not just `[bob]`: issue #506 closed the gap this
    // test used to document — `guild.created` is now a kind
    // `crate::projections::guild_rosters::decode` recognizes too, so
    // alice's owner membership (implied by that event's own `owner` field,
    // never a separate `guild.member_added`) lands here alongside bob's
    // real `guild.member_added` from `/join`.
    let guild_member_ids: Vec<Uuid> = sqlx::query(
        "SELECT identity_id FROM indexer_guild_members WHERE guild_id = $1 ORDER BY identity_id",
    )
    .bind(Uuid::parse_str(&guild_id).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get("identity_id"))
    .collect();
    let mut expected_member_ids = vec![alice_id, bob_id];
    expected_member_ids.sort();
    assert_eq!(
        guild_member_ids, expected_member_ids,
        "rebuilt indexer_guild_members must contain the owner and the joiner"
    );
}

/// Replaying the exact same ledger history a second time — a second
/// disaster, or a second `avalon rebuild-index` run — must be a no-op:
/// same snapshot, not a drift or a duplicate-row error.
#[tokio::test]
#[ignore]
async fn replay_onto_rebuilt_index_is_noop() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (_alice_id, alice_token) = register_and_login(&http, &base).await;
    auth(http.patch(format!("{base}/me")), &alice_token)
        .json(&serde_json::json!({ "bio": "idempotency fixture" }))
        .send()
        .await
        .expect("PATCH /me failed")
        .error_for_status()
        .unwrap();

    wait_for_outbox_drain(&pool).await;

    let chain = chain(&pool).await;
    avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool)
        .await
        .expect("first rebuild failed");
    let first_snapshot = snapshot_all_projections(&pool).await;

    avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool)
        .await
        .expect("second rebuild failed");
    let second_snapshot = snapshot_all_projections(&pool).await;

    assert_eq!(
        first_snapshot, second_snapshot,
        "rebuilding twice from the same ledger history must produce an identical snapshot"
    );
}

/// Issue #43's acceptance criterion that the exclusion list is a
/// deliberate, checked decision, not an accident: every table this repo's
/// disaster-recovery doc names as out of scope
/// (`docs/architecture/disaster-recovery.md`) must NOT appear in
/// [`PROJECTION_TABLES`] — a promised-durable projection and an explicitly
/// ephemeral/credential table must never be the same table.
#[test]
fn rebuild_excludes_ephemeral_tables() {
    const EXCLUDED: &[&str] = &[
        "sessions",
        "identity_keys",
        "identity_signing_keys",
        "webauthn_ceremonies",
        "presence_preferences",
    ];
    for table in EXCLUDED {
        assert!(
            !PROJECTION_TABLES.contains(table),
            "{table} is documented as excluded from rebuild but is in PROJECTION_TABLES"
        );
    }
}
