//! SDK client for `POST /issuers/registration-challenge` /
//! `POST /issuers/register` (#481, implementing the ADR decided in #479),
//! wired through #483's own target-network declaration + mismatch check:
//! [`AvalonClient::register_issuer`] never sends a registration request
//! without first independently verifying (via [`crate::network::verify_network`])
//! that the server it's about to write to is actually the network the
//! caller declared intent for. This is the SDK's own belt-and-suspenders
//! half of `crates/server/src/issuer_registration.rs::register_issuer`'s
//! `declared_network_id` check — a client-side mistake should never even
//! reach the server, let alone rely on the server to catch it.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::network::{check_target_network, NetworkTargetError, TargetNetwork};
use crate::{AvalonClient, SdkError};

/// Everything that can go wrong registering an issuer — distinct from the
/// bare [`SdkError`] taxonomy because a target-network mismatch is refused
/// entirely client-side, before any HTTP request that could produce one.
#[derive(Debug, thiserror::Error)]
pub enum IssuerRegistrationError {
    /// #483's own check: the declared [`TargetNetwork`] doesn't match (or
    /// couldn't be confirmed to match) the server's independently verified
    /// network. No registration request was sent.
    #[error(transparent)]
    NetworkTarget(#[from] NetworkTargetError),
    /// `AvalonConfig::signing_key` was `None` — registering an issuer
    /// requires the private key whose public half is being registered.
    #[error("no signing key configured — set AvalonConfig::signing_key")]
    MissingSigningKey,
    /// Any other failure talking to `avalon-server` itself.
    #[error(transparent)]
    Sdk(#[from] SdkError),
}

/// The result of a successful [`AvalonClient::register_issuer`] call.
#[derive(Debug, Clone)]
pub struct IssuerRegistration {
    /// The `issuer_ref` now registered on `network_id`.
    pub issuer_ref: String,
    /// The exact `network_id` this registration was admitted on — the
    /// verified entry's own `network_id`, not merely echoing back what the
    /// caller declared.
    pub network_id: String,
    /// When the server recorded this registration.
    pub registered_at: OffsetDateTime,
}

#[derive(Deserialize)]
struct ChallengeWire {
    challenge_id: Uuid,
    nonce: String,
}

#[derive(Deserialize)]
struct RegisterWire {
    issuer_ref: String,
    #[serde(with = "time::serde::rfc3339")]
    registered_at: OffsetDateTime,
}

/// Must produce exactly the bytes
/// `crates/server/src/issuer_registration.rs::proof_of_possession_message`
/// reconstructs — see that function's doc comment for why `issuer_ref` and
/// `declared_network_id` are bound into the signed message, not just the
/// raw nonce.
fn proof_of_possession_message(
    issuer_ref: &str,
    declared_network_id: &str,
    nonce: &[u8],
) -> Vec<u8> {
    format!(
        "avalon:issuer.registered:v1:{issuer_ref}:{declared_network_id}:{}",
        BASE64.encode(nonce)
    )
    .into_bytes()
}

impl AvalonClient {
    /// Registers this client's configured signing key as `issuer_ref` on
    /// the network it declares intent for via `target` — #483's own
    /// invariant, enforced before any request reaches the server: `target`
    /// is checked against [`AvalonClient::verify_network`]'s result first,
    /// and a mismatch or unverifiable network is a hard
    /// [`IssuerRegistrationError::NetworkTarget`], never a silent proceed.
    ///
    /// Idempotent server-side (re-registering an already-registered key
    /// just updates its `issuer_ref`), so this is safe to retry as a whole
    /// on failure.
    pub async fn register_issuer(
        &self,
        issuer_ref: &str,
        target: TargetNetwork,
    ) -> Result<IssuerRegistration, IssuerRegistrationError> {
        let signing_key_bytes = self
            .config
            .signing_key
            .ok_or(IssuerRegistrationError::MissingSigningKey)?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);

        let status = self.verify_network().await;
        let declared_network_id = check_target_network(&status, &target)?.network_id.clone();

        let issuer_pubkey = BASE64.encode(signing_key.verifying_key().to_bytes());

        let challenge_response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.post(format!(
                "{}/issuers/registration-challenge",
                self.config.server_url
            ))
            .json(&serde_json::json!({ "issuer_pubkey": issuer_pubkey }))
        })
        .await?;
        if !challenge_response.status().is_success() {
            return Err(crate::http::map_error_response(challenge_response)
                .await
                .into());
        }
        let challenge: ChallengeWire = challenge_response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        let nonce = BASE64.decode(&challenge.nonce).map_err(|_| {
            SdkError::Protocol("registration challenge nonce was not valid base64".to_string())
        })?;

        let message = proof_of_possession_message(issuer_ref, &declared_network_id, &nonce);
        let signature = signing_key.sign(&message);

        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!("{}/issuers/register", self.config.server_url))
                .json(&serde_json::json!({
                    "issuer_pubkey": issuer_pubkey,
                    "issuer_ref": issuer_ref,
                    "declared_network_id": declared_network_id,
                    "challenge_id": challenge.challenge_id,
                    "proof_of_possession_signature": BASE64.encode(signature.to_bytes()),
                }))
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await.into());
        }
        let body: RegisterWire = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        Ok(IssuerRegistration {
            issuer_ref: body.issuer_ref,
            network_id: declared_network_id,
            registered_at: body.registered_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::{AvalonConfig, RetryConfig};

    fn client_with_key(server_url: String, signing_key: &SigningKey) -> AvalonClient {
        AvalonClient::new(AvalonConfig {
            server_url,
            integrator_credential_key_id: "test-integrator".to_string(),
            integrator_slug: None,
            signing_key: Some(signing_key.to_bytes()),
            retry: RetryConfig {
                max_retries: 0,
                base_delay: std::time::Duration::from_millis(1),
                request_timeout: std::time::Duration::from_secs(5),
            },
        })
    }

    async fn mount_verified_sth(server: &MockServer, signing_key: &SigningKey, network_id: &str) {
        let sth = avalon_protocol::sth::sign_tree_head(
            signing_key,
            "test-key",
            1,
            &"ab".repeat(32),
            network_id,
            OffsetDateTime::UNIX_EPOCH,
        );
        Mock::given(method("GET"))
            .and(path("/ledger/sth/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tree_size": sth.tree_size,
                "root_hash": sth.root_hash,
                "network_id": sth.network_id,
                "signing_key_id": sth.signing_key_id,
                "signature": sth.signature,
                "created_at": "1970-01-01T00:00:00Z",
            })))
            .mount(server)
            .await;
    }

    /// This crate's own build only bundles `avalon-dev-local` as a pinned
    /// trust anchor (see `docs/trusted-networks.json`), so
    /// `AvalonClient::verify_network` reports any other `network_id` —
    /// including one whose STH is genuinely well-formed and self-
    /// consistent, as mounted here — as `UnknownNetwork`. Registration
    /// must refuse rather than proceed just because the declared
    /// `network_id` happens to textually match what the server claims: an
    /// unpinned network is never treated as verified, matching
    /// `check_target_network`'s own invariant (network.rs's own unit
    /// tests cover the `Verified`-and-matching success path directly,
    /// since exercising it here would require polluting the real
    /// checked-in trust-anchor list).
    #[tokio::test]
    async fn register_issuer_refuses_an_unpinned_network_even_if_the_network_id_matches() {
        let settlement_key = SigningKey::generate(&mut rand::rng());
        let issuer_key = SigningKey::generate(&mut rand::rng());
        let server = MockServer::start().await;
        mount_verified_sth(&server, &settlement_key, "avalon-dev-1").await;
        // No mock for /issuers/registration-challenge or /issuers/register:
        // the network check must refuse before either is ever called.

        let client = client_with_key(server.uri(), &issuer_key);
        let target = TargetNetwork::NetworkId("avalon-dev-1".to_string());
        let result = client.register_issuer("game:ashen-realms", target).await;

        assert!(matches!(
            result,
            Err(IssuerRegistrationError::NetworkTarget(
                NetworkTargetError::Unverified { .. }
            ))
        ));
    }

    #[tokio::test]
    async fn register_issuer_requires_a_configured_signing_key() {
        let server = MockServer::start().await;
        let client = AvalonClient::new(AvalonConfig {
            server_url: server.uri(),
            integrator_credential_key_id: "test-integrator".to_string(),
            integrator_slug: None,
            signing_key: None,
            retry: RetryConfig {
                max_retries: 0,
                base_delay: std::time::Duration::from_millis(1),
                request_timeout: std::time::Duration::from_secs(5),
            },
        });

        let err = client
            .register_issuer(
                "game:ashen-realms",
                TargetNetwork::NetworkId("avalon-dev-1".to_string()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, IssuerRegistrationError::MissingSigningKey));
    }
}
