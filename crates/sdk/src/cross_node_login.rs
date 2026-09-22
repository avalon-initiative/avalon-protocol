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

use crate::{AvalonClient, SdkError, Session};

/// How long a grant is valid for after `issued_at`, at mint time — the
/// verifying node independently re-checks `expires_at` itself. Short: a
/// grant is a one-shot approval consumed once by
/// `POST /auth/cross-node/submit`, not a re-presented claim.
pub const DEFAULT_TTL_SECONDS: i64 = 60;

/// A self-signed assertion that a human, shown `requesting_context`, just
/// approved logging `identity_id` (acting through `signing_key_id`) into
/// `destination_base_url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossNodeLoginGrant {
    /// The identity being logged in.
    pub identity_id: Uuid,
    /// Which of the identity's (possibly several) signing keys approved
    /// this.
    pub signing_key_id: Uuid,
    /// The node this grant is good for logging into, and nowhere else —
    /// inside the signed bytes, so an approved grant can't be relayed to a
    /// node the human never saw.
    pub destination_base_url: String,
    /// The human-legible context shown to the approver before they
    /// approved — carried in the signed bytes so the grant itself is
    /// evidence of what was shown, not just a claim about it.
    pub requesting_context: String,
    /// Anti-replay: unique per grant, checked against a consumed-nonce
    /// table by the verifying node.
    pub nonce: Uuid,
    /// When this grant was minted.
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    /// When it stops being accepted.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Lowercase hex-encoded Ed25519 signature over [`signing_bytes`].
    pub signature: String,
}

impl CrossNodeLoginGrant {
    /// The bytes this grant's own `signature` field covers — what both the
    /// approving client and the verifying node compute independently.
    pub fn signing_bytes(&self) -> Vec<u8> {
        signing_bytes(
            self.identity_id,
            self.signing_key_id,
            &self.destination_base_url,
            &self.requesting_context,
            self.nonce,
            self.issued_at,
            self.expires_at,
        )
    }
}

/// The exact bytes a [`CrossNodeLoginGrant`]'s signature covers —
/// deliberately excludes `signature` itself and includes every other field,
/// so a signature can never be replayed against a different identity, key,
/// destination, context, or validity window than the one it was actually
/// produced for.
///
/// Must stay byte-for-byte identical to the verifying node's own
/// construction (`avalon_protocol::cross_node_login::signing_bytes`) and to
/// the C#/TypeScript SDKs' — `conformance/vectors/cross-node-login.json`
/// is what proves it still is.
pub fn signing_bytes(
    identity_id: Uuid,
    signing_key_id: Uuid,
    destination_base_url: &str,
    requesting_context: &str,
    nonce: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
) -> Vec<u8> {
    format!(
        "avalon:cross-node-login:v1:{identity_id}:{signing_key_id}:{destination_base_url}:{requesting_context}:{nonce}:{}:{}",
        issued_at.unix_timestamp(),
        expires_at.unix_timestamp(),
    )
    .into_bytes()
}

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

/// The approver-side read backing an approval screen — mirrors
/// `crates/server/src/cross_node_login.rs::LookupCrossNodeLoginResponse`
/// at the wire level. Never carries `request_code` (the polling device's
/// own bearer credential, not the approver's business).
#[derive(Debug, Clone, Deserialize)]
pub struct CrossNodeLoginLookup {
    /// One of `"pending"`, `"denied"`, `"expired"`, `"approved"` — an
    /// approval screen only ever meaningfully acts on `"pending"`.
    pub status: String,
    /// The requesting node's own context to show a human before they
    /// decide whether to approve.
    pub requesting_context: String,
    /// Seconds until this request expires if it's never approved.
    pub expires_in: i64,
    /// Whether this node (the one being logged into) resolves to a real,
    /// registered integrator or known network anchor — issue #649,
    /// implementing #642's decided phishing-context requirement. Never a
    /// hard gate; an unverified requester still gets a prompt, just a
    /// clearly flagged one.
    pub integrator_verified: bool,
    /// A real registered display name — only present when
    /// `integrator_verified` is `true`.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// [`AvalonClient::deny_cross_node_login`]'s response — mirrors
/// `crates/server/src/cross_node_login.rs::DenyResponse`.
#[derive(Debug, Clone, Deserialize)]
pub struct CrossNodeLoginDenial {
    /// Always `"denied"` on success.
    pub status: String,
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

    /// `GET /auth/cross-node/lookup?user_code=...` (closed out by
    /// #741/#749) — the approver's own half of the flow: reads real
    /// context for a `user_code` *before* deciding whether to approve or
    /// [`AvalonClient::deny_cross_node_login`] it. Unauthenticated —
    /// there's routinely no session yet on the node an approver reaches
    /// this from.
    pub async fn lookup_cross_node_login(
        &self,
        user_code: &str,
    ) -> Result<CrossNodeLoginLookup, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/auth/cross-node/lookup", self.config.server_url))
                .query(&[("user_code", user_code)])
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    /// `POST /auth/cross-node/deny` (closed out by #741/#749) — the
    /// approver denying a pending cross-node login by its `user_code`.
    /// Deliberately unauthenticated (see
    /// `crates/server/src/cross_node_login.rs::deny`'s own doc comment: a
    /// denial grants nothing, so the worst case of a guessed code is
    /// griefing one's own pending request, not a security bypass).
    pub async fn deny_cross_node_login(
        &self,
        user_code: &str,
    ) -> Result<CrossNodeLoginDenial, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!("{}/auth/cross-node/deny", self.config.server_url))
                .json(&serde_json::json!({ "user_code": user_code }))
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    /// `GET /identities/{id}/locations` (closed out by #741/#749) —
    /// every location (server base URL) currently advertised for
    /// `identity_id`, the DHT-backed set a requesting node resolves
    /// *before* cross-node login can even start (there is routinely no
    /// session yet at this point — see this method's own unauthenticated
    /// posture, matching `crates/server/src/identity_locator.rs::get_locations`).
    /// Empty, never an error, for an identity with no advertised location
    /// (e.g. no DHT identity configured on the node it's home to).
    pub async fn identity_locations(&self, identity_id: Uuid) -> Result<Vec<String>, SdkError> {
        #[derive(Deserialize)]
        struct LocationsResponse {
            locations: Vec<String>,
        }

        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/identities/{identity_id}/locations",
                self.config.server_url
            ))
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let body: LocationsResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(body.locations)
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

#[cfg(test)]
mod approver_tests {
    use super::*;

    #[test]
    fn cross_node_login_lookup_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "status": "pending",
            "requesting_context": "https://ashen-realms.example",
            "expires_in": 300,
            "integrator_verified": true,
            "display_name": "Ashen Realms",
        });
        let lookup: CrossNodeLoginLookup = serde_json::from_value(raw).unwrap();
        assert_eq!(lookup.status, "pending");
        assert!(lookup.integrator_verified);
        assert_eq!(lookup.display_name.as_deref(), Some("Ashen Realms"));
    }

    #[test]
    fn cross_node_login_denial_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({ "status": "denied" });
        let denial: CrossNodeLoginDenial = serde_json::from_value(raw).unwrap();
        assert_eq!(denial.status, "denied");
    }
}
