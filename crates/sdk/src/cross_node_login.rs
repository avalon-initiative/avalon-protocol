//! `AvalonClient::cross_node_login()` (epic #623, issue #637) — the
//! ergonomic wrapper around #634's cross-node login lifecycle
//! (`crates/server/src/cross_node_login.rs`), mirroring `device_login`'s
//! own start-then-poll shape almost exactly: from a caller's perspective
//! both flows look identical (`POST .../start` on this node, show the user
//! a code, poll until approved, get back a real [`Session`]). The one
//! structural difference: `POST /auth/cross-node/start` returns no
//! `verification_uri` the way `device_login::DeviceLogin` does — there is
//! no Hub route for cross-node approval yet (#639, not built) — just a
//! `user_code` and `requesting_context` to show however this integrator's
//! own UI displays a pairing code.
//!
//! **Same-device fast path** ([`AvalonClient::submit_cross_node_login_grant`]):
//! epic #623's own scope note draws a real distinction between the
//! cross-device dance above (needed when the approving human is somewhere
//! that genuinely doesn't hold the identity's signing key) and a caller
//! that already does — which is not the common case for this SDK's usual
//! integrator-backend callers (who never hold a *player's* own key), but a
//! real one for a deployment that directly controls some identity's key
//! material (e.g. a service/bot identity). That caller can mint, sign, and
//! submit a grant in one call, skipping the start/poll dance entirely.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use avalon_protocol::cross_node_login::{signing_bytes, CrossNodeLoginGrant, DEFAULT_TTL_SECONDS};

use crate::{AvalonClient, SdkError, Session};

/// Same floor `device_login`'s own `DEFAULT_POLL_INTERVAL_SECONDS`
/// documents, for the same reason — a server response should always carry
/// its own `poll_interval`, this is only a fallback.
const DEFAULT_POLL_INTERVAL_SECONDS: i64 = 5;
/// Same cap `device_login::MAX_POLL_INTERVAL_SECONDS` documents.
const MAX_POLL_INTERVAL_SECONDS: i64 = 60;

#[derive(Deserialize)]
struct StartCrossNodeLoginResponse {
    request_code: String,
    user_code: String,
    requesting_context: String,
    expires_in: i64,
    poll_interval: i64,
}

#[derive(Deserialize)]
struct PollCrossNodeLoginResponse {
    status: String,
    token: Option<String>,
}

#[derive(Serialize)]
struct SubmitGrantRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    user_code: Option<&'a str>,
    grant: &'a CrossNodeLoginGrant,
}

#[derive(Deserialize)]
struct SubmitGrantResponse {
    token: Option<String>,
}

/// A pending cross-node login (epic #623), returned by
/// [`AvalonClient::cross_node_login`]. Borrows the `AvalonClient` it was
/// started from, same shape `device_login::DeviceLogin` already
/// establishes.
pub struct CrossNodeLogin<'a> {
    client: &'a AvalonClient,
    request_code: String,
    /// Short, human-typeable code — show it however this integrator's own
    /// UI displays a pairing code. Unlike
    /// `device_login::DeviceLogin::verification_uri`, there's no
    /// ready-made Hub route to point at yet (#639, not built).
    pub user_code: String,
    /// The requesting node's own context, meant to be shown by whatever
    /// eventually approves this (Hub/mobile-hub, once built) so a human
    /// can tell what they're approving login to.
    pub requesting_context: String,
    /// Seconds until this request expires if it's never approved.
    pub expires_in: i64,
    poll_interval: i64,
}

impl AvalonClient {
    /// Starts a cross-node login (epic #623) via `POST /auth/cross-node/start`
    /// against this client's own `server_url` — the node being logged
    /// into, which may not be the identity's "home" node at all.
    pub async fn cross_node_login(&self) -> Result<CrossNodeLogin<'_>, SdkError> {
        // No idempotency key: same reasoning `device_login::login`'s own
        // doc comment gives — a retried start mints a second, independent
        // request rather than replaying the first.
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!("{}/auth/cross-node/start", self.config.server_url))
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let body: StartCrossNodeLoginResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        Ok(CrossNodeLogin {
            client: self,
            request_code: body.request_code,
            user_code: body.user_code,
            requesting_context: body.requesting_context,
            expires_in: body.expires_in,
            poll_interval: if body.poll_interval > 0 {
                body.poll_interval
            } else {
                DEFAULT_POLL_INTERVAL_SECONDS
            },
        })
    }

    /// The same-device fast path (see module doc comment): mints, signs,
    /// and submits a `CrossNodeLoginGrant` directly against this client's
    /// own `server_url`, for a caller that already holds `identity_id`'s
    /// real Ed25519 event-signing key (`signing_key`, identified by
    /// `signing_key_id`) — skipping `cross_node_login`/`CrossNodeLogin::wait`
    /// entirely. Resolves to a real [`Session`] the same way `wait` does,
    /// via [`AvalonClient::authenticate`].
    pub async fn submit_cross_node_login_grant(
        &self,
        identity_id: Uuid,
        signing_key_id: Uuid,
        signing_key: &SigningKey,
    ) -> Result<Session, SdkError> {
        let issued_at = OffsetDateTime::now_utc();
        let expires_at = issued_at + Duration::seconds(DEFAULT_TTL_SECONDS);
        let nonce = Uuid::new_v4();
        let destination_base_url = self.config.server_url.clone();
        // No Hub-driven context yet to show a human (#639) — the
        // destination itself is the most honest context this caller can
        // supply on its own behalf.
        let requesting_context = destination_base_url.clone();

        let bytes = signing_bytes(
            identity_id,
            signing_key_id,
            &destination_base_url,
            &requesting_context,
            nonce,
            issued_at,
            expires_at,
        );
        let signature = signing_key.sign(&bytes);
        let grant = CrossNodeLoginGrant {
            identity_id,
            signing_key_id,
            destination_base_url,
            requesting_context,
            nonce,
            issued_at,
            expires_at,
            signature: hex::encode(signature.to_bytes()),
        };

        let request = SubmitGrantRequest {
            user_code: None,
            grant: &grant,
        };
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!("{}/auth/cross-node/submit", self.config.server_url))
                .json(&request)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let body: SubmitGrantResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        let token = body.token.ok_or_else(|| {
            SdkError::Protocol("same-device submit response was missing a token".to_string())
        })?;
        self.authenticate(&token).await
    }
}

impl CrossNodeLogin<'_> {
    /// Drives `POST /auth/cross-node/poll` to completion — identical
    /// backoff shape to `device_login::DeviceLogin::wait`. Resolves to a
    /// real [`Session`] on `approved`, or a typed [`SdkError`] on
    /// `denied`/`expired`.
    pub async fn wait(&self) -> Result<Session, SdkError> {
        let mut interval = self.poll_interval.max(1);

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval as u64)).await;

            let response =
                crate::http::send(&self.client.http, &self.client.config.retry, true, |c| {
                    c.post(format!(
                        "{}/auth/cross-node/poll",
                        self.client.config.server_url
                    ))
                    .bearer_auth(&self.request_code)
                })
                .await?;
            if !response.status().is_success() {
                return Err(crate::http::map_error_response(response).await);
            }
            let body: PollCrossNodeLoginResponse = response
                .json()
                .await
                .map_err(|e| SdkError::Protocol(e.to_string()))?;

            match body.status.as_str() {
                "pending" => continue,
                "slow_down" => {
                    interval = (interval * 2).min(MAX_POLL_INTERVAL_SECONDS);
                    continue;
                }
                "denied" => return Err(SdkError::CrossNodeLoginDenied),
                "approved" => {
                    // Single-use delivery, same invariant
                    // `device_login::DeviceLogin::wait`'s own doc comment
                    // documents for the identical reason.
                    let token = body.token.ok_or_else(|| {
                        SdkError::Protocol("approved poll response was missing a token".to_string())
                    })?;
                    return self.client.authenticate(&token).await;
                }
                _ => return Err(SdkError::CrossNodeLoginExpired),
            }
        }
    }
}
