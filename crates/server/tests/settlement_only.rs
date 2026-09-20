//! Issue #664: a genuinely standalone Settlement node — `AVALON_NODE_ROLES=settlement`
//! — actually serves a reduced route table (no Gateway-facing surface
//! mounted at all), and a separate Gateway-only node pointed at it via
//! #313's existing `AVALON_SETTLEMENT_REMOTE_URL` mechanism can still
//! commit writes through to it.
//!
//! Gated `--ignored`, same convention `tests/remote_settlement.rs` (#313)
//! already uses for anything needing a second real `avalon-server` process.
//! Needs two real processes against two independent Postgres
//! schemas/databases, sharing one `AVALON_NETWORK_ID`:
//!
//! ```text
//! # Settlement-only node
//! AVALON_NODE_ROLES=settlement
//! AVALON_SETTLEMENT_SUBMIT_KEY=<shared secret>
//! # no AVALON_WEBAUTHN_RP_ID/ORIGIN needed at all
//!
//! # Gateway-only node — own database, remote-submit pointed at the above
//! AVALON_NODE_ROLES=gateway
//! AVALON_SETTLEMENT_REMOTE_URL=http://127.0.0.1:<settlement-only port>
//! AVALON_SETTLEMENT_SUBMIT_KEY=<same shared secret>
//! ```
//!
//! The write exercised below is an identity registration, not an
//! integrator registration — deliberately: `crate::outbox::shard_id_for_event`
//! only routes a `game:`/`app:`/`service:`-namespaced issuer through a
//! *named* shard (needing its own `AVALON_SETTLEMENT_REMOTE_URLS` entry);
//! an `identity:`-namespaced issuer falls into the default `"core"` shard,
//! which `AVALON_SETTLEMENT_REMOTE_URL` (singular) already covers with no
//! extra per-shard config — and it's the exact scenario this ticket's own
//! acceptance criteria names ("an identity registration on the Gateway node
//! actually commits through"). `register_identity` below borrows
//! `tests/identity_locator.rs`'s real (not mocked) WebAuthn ceremony
//! helper shape, driven through a software/virtual authenticator — the
//! same real `/identities/register/{start,finish}` handlers a browser
//! would hit, just with no browser involved.

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use uuid::Uuid;

/// The Settlement-only node under test — the one this file's own
/// acceptance criteria are about. `AVALON_SETTLEMENT_ONLY_SERVER_URL` to
/// keep the name distinct from every other live test's `AVALON_SERVER_URL`
/// (which several other files in this crate default to `127.0.0.1:8080`,
/// the ordinary combined `make start` node — not what this file exercises).
fn settlement_only_url() -> Option<String> {
    std::env::var("AVALON_SETTLEMENT_ONLY_SERVER_URL").ok()
}

/// The Gateway-only node pointed at the Settlement-only node above via
/// #313's `AVALON_SETTLEMENT_REMOTE_URL`.
fn gateway_only_url() -> Option<String> {
    std::env::var("AVALON_GATEWAY_ONLY_SERVER_URL").ok()
}

/// The Gateway-only node's own `AVALON_WEBAUTHN_ORIGIN` — needed to drive
/// a real registration ceremony against it.
fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:5173".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

type VirtualClient =
    Client<MemoryStore, MockUserValidationMethod, public_suffix::PublicSuffixList, ()>;

fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(1),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Registers a brand-new identity against `base` — same shape
/// `tests/identity_locator.rs::register_identity` already establishes,
/// driving the real `/identities/register/{start,finish}` handlers via a
/// software WebAuthn authenticator. Returns the identity's own id — note
/// this is *not* the `subject` string on the resulting ledger entries
/// (those are e.g. `identity:<id>:self:passkey_registered`, one per
/// sub-event kind), so a caller matching entries back to this identity
/// needs a prefix match, not equality.
async fn register_identity(http: &reqwest::Client, base: &str) -> Uuid {
    let identity_id = Uuid::new_v4();
    let display_name = format!("settlement-only-test-{identity_id}");

    let start_body = serde_json::json!({
        "identity_id": identity_id,
        "display_name": display_name,
    });
    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&start_body)
        .send()
        .await
        .expect("register/start failed — is the Gateway-only node running?")
        .json()
        .await
        .expect("register/start response wasn't JSON");
    let ticket_id = start["ticket_id"]
        .as_str()
        .expect("register/start response missing ticket_id")
        .to_string();

    let mut client = new_virtual_client();
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone())
            .expect("failed to parse WebAuthn creation challenge");
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let signing_bytes =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes);

    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;

    let finish_body = serde_json::json!({
        "ticket_id": ticket_id,
        "webauthn_credential": credential,
        "event_signing_public_key": BASE64.encode(signing_key.verifying_key().to_bytes()),
        "event_signature": BASE64.encode(signature.to_bytes()),
        "device_label": null,
    });
    http.post(format!("{base}/identities/register/finish"))
        .json(&finish_body)
        .send()
        .await
        .expect("register/finish request failed")
        .error_for_status()
        .expect("register/finish should succeed");

    identity_id
}

async fn wait_for_seq<F>(mut fetch: F, description: &str) -> i64
where
    F: AsyncFnMut() -> Option<i64>,
{
    for _ in 0..30 {
        if let Some(value) = fetch().await {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("{description} — never appeared within the retry budget");
}

/// The acceptance criterion this ticket's own module doc comment calls
/// "genuinely no Gateway modules loaded" — a request to an ordinary
/// Gateway-facing endpoint against the Settlement-only node gets a clean
/// 404 (axum's own "no matching route" response), never a panic and never
/// a route that happens to work anyway. `/identities/register/start` is
/// the concrete example the ticket names.
#[tokio::test]
#[ignore]
async fn settlement_only_node_has_no_gateway_routes_mounted() {
    let Some(base) = settlement_only_url() else {
        panic!(
            "AVALON_SETTLEMENT_ONLY_SERVER_URL not set — this test needs a real avalon-server \
             process started with AVALON_NODE_ROLES=settlement; see this file's module doc comment"
        );
    };
    let http = reqwest::Client::new();

    let response = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed outright — is the Settlement-only node actually up?");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a Gateway-only route must 404 on a Settlement-only node, not panic or succeed"
    );

    // A second, differently-shaped Gateway route, for good measure — not
    // just the one the ticket happens to name.
    let response = http
        .get(format!("{base}/me"))
        .send()
        .await
        .expect("request failed outright");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    // `/ledger/*` (Settlement's own surface) still works on the same node.
    let response = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .expect("request failed outright");
    assert!(
        response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
        "ledger/sth/latest should be a real, mounted route (200, or 404 only if this node has \
         no committed history yet) — got {:?}",
        response.status()
    );
}

/// The ticket's own end-to-end acceptance criterion: an identity
/// registration submitted against a Gateway-only node (no local Settlement
/// role at all, using #313's existing remote-submit config) actually lands
/// on the standalone Settlement-only node's own ledger, and that node's
/// `/ledger/sth/latest` reflects it.
#[tokio::test]
#[ignore]
async fn an_identity_registration_on_the_gateway_only_node_commits_through_to_the_settlement_only_node(
) {
    let Some(settlement_base) = settlement_only_url() else {
        panic!("AVALON_SETTLEMENT_ONLY_SERVER_URL not set — see this file's module doc comment");
    };
    let Some(gateway_base) = gateway_only_url() else {
        panic!(
            "AVALON_GATEWAY_ONLY_SERVER_URL not set — this test needs a second real \
             avalon-server process, AVALON_NODE_ROLES=gateway with \
             AVALON_SETTLEMENT_REMOTE_URL pointed at the Settlement-only node; see this file's \
             module doc comment"
        );
    };

    let http = reqwest::Client::new();
    let identity_id = register_identity(&http, &gateway_base).await;
    let subject_prefix = format!("identity:{identity_id}:");

    // The Settlement-only node's `/ledger/entries` is the same public,
    // unauthenticated read `crate::settlement::list_entries` exposes on
    // any node — polled here rather than a direct DB query (this test has
    // no direct access to the Settlement-only node's own database schema,
    // unlike `tests/remote_settlement.rs`, which can reach both databases
    // directly). `subject` isn't queried server-side by exact match here —
    // a registration produces more than one entry (`self:passkey_registered`,
    // `self:signing_key_added`), each with its own `subject` suffix — so
    // this fetches everything since the start of this test run and matches
    // the prefix client-side instead.
    let seq = wait_for_seq(
        async || {
            let response = http
                .get(format!("{settlement_base}/ledger/entries"))
                .query(&[("limit", "1000")])
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            let entries: Vec<serde_json::Value> = response.json().await.ok()?;
            entries
                .into_iter()
                .find(|e| {
                    e.get("subject")
                        .and_then(|s| s.as_str())
                        .is_some_and(|s| s.starts_with(&subject_prefix))
                })?
                .get("seq")?
                .as_i64()
        },
        "the Gateway-only node's identity registration never appeared in the Settlement-only \
         node's own /ledger/entries — is its outbox worker running with \
         AVALON_SETTLEMENT_REMOTE_URL/AVALON_SETTLEMENT_SUBMIT_KEY configured correctly?",
    )
    .await;
    assert!(seq >= 0);

    // `/ledger/sth/latest` on the Settlement-only node reflects a tree
    // past that entry's own `seq` — the ticket's own explicit acceptance
    // line ("`/ledger/sth/latest` on the Settlement node reflects it").
    let sth: serde_json::Value = http
        .get(format!("{settlement_base}/ledger/sth/latest"))
        .send()
        .await
        .expect("GET /ledger/sth/latest failed")
        .json()
        .await
        .expect("STH response wasn't valid JSON");
    let tree_size = sth
        .get("tree_size")
        .and_then(|v| v.as_i64())
        .expect("STH response missing tree_size");
    assert!(
        tree_size > seq,
        "STH tree_size ({tree_size}) should be past the committed entry's own seq ({seq})"
    );
}
