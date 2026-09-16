//! Exercises `AvalonClient::register_issuer` (issue #483, on top of #481's
//! server-side endpoint) against a real, running `avalon-server`. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`. The local dev deployment's `network_id` (`avalon-dev-local`) is
//! also the one entry `docs/trusted-networks.json` bundles, and a local
//! `.env`'s `AVALON_SETTLEMENT_SIGNING_KEY` is the private half of that
//! entry's pinned `verify_key` — so, uniquely among this crate's live
//! tests, `verify_network()` against a real local dev server actually
//! reaches `NetworkTrustStatus::Verified`, letting this test exercise the
//! true success path end to end, not just the client-side refusal paths
//! `crates/sdk/src/issuer_registration.rs`'s unit tests cover.

use avalon_sdk::issuer_registration::IssuerRegistrationError;
use avalon_sdk::network::{NetworkTargetError, TargetNetwork, TargetNetworkTier};
use avalon_sdk::{AvalonClient, AvalonConfig};
use ed25519_dalek::SigningKey;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn client_with_key(signing_key: &SigningKey) -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: String::new(),
        integrator_slug: None,
        signing_key: Some(signing_key.to_bytes()),
        retry: Default::default(),
    })
}

#[tokio::test]
#[ignore]
async fn register_issuer_succeeds_end_to_end_when_the_declared_network_id_matches() {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let issuer_ref = format!("game:sdk-network-test-{}", uuid::Uuid::new_v4().simple());

    let registration = client_with_key(&signing_key)
        .register_issuer(
            &issuer_ref,
            TargetNetwork::NetworkId("avalon-dev-local".to_string()),
        )
        .await
        .expect("register_issuer should succeed against the real local dev network");

    assert_eq!(registration.issuer_ref, issuer_ref);
    assert_eq!(registration.network_id, "avalon-dev-local");
}

#[tokio::test]
#[ignore]
async fn register_issuer_is_idempotent_and_updates_the_issuer_ref_on_re_registration() {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let first_ref = format!("game:sdk-network-test-{}", uuid::Uuid::new_v4().simple());
    let second_ref = format!("game:sdk-network-test-{}", uuid::Uuid::new_v4().simple());
    let client = client_with_key(&signing_key);
    let target = || TargetNetwork::NetworkId("avalon-dev-local".to_string());

    client
        .register_issuer(&first_ref, target())
        .await
        .expect("first registration should succeed");
    let second = client
        .register_issuer(&second_ref, target())
        .await
        .expect("re-registering the same key should update issuer_ref, not error");

    assert_eq!(second.issuer_ref, second_ref);
}

/// The real target-network mismatch case (#483's own reason for existing):
/// the server really is `avalon-dev-local`, verified via a real STH, but the
/// caller declared a different real target — refused client-side, so no
/// registration request is ever sent (confirmed indirectly: a mismatched
/// `issuer_ref` would otherwise have been accepted, since the server-side
/// gate is a plain string comparison against its own `network_id`).
#[tokio::test]
#[ignore]
async fn register_issuer_refuses_a_real_declared_mismatch_against_the_real_verified_network() {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let issuer_ref = format!("game:sdk-network-test-{}", uuid::Uuid::new_v4().simple());

    let err = client_with_key(&signing_key)
        .register_issuer(&issuer_ref, TargetNetwork::Env(TargetNetworkTier::Mainnet))
        .await
        .expect_err("declaring mainnet against a real dev server must be refused");

    assert!(matches!(
        err,
        IssuerRegistrationError::NetworkTarget(NetworkTargetError::Mismatch { .. })
    ));
}
