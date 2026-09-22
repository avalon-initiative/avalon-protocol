//! Social recovery's *request-initiation* half (issue #201, closed out by
//! #741/#747) — starting and finishing a real WebAuthn registration
//! ceremony for a brand-new device/key when every one of an identity's
//! existing passkeys is lost, then finalizing the recovery once its
//! guardian threshold and mandatory public delay are both satisfied. See
//! `crates/server/src/recovery.rs`'s own module doc comment for the full
//! state machine and abuse-resistance measures; `account::recovery` (on
//! [`crate::AccountSession`]) already covers the *other* half — an
//! already-logged-in identity configuring its own guardians, and a
//! guardian approving/cancelling someone else's in-flight request.
//!
//! **Necessarily unauthenticated.** Every method here lives on
//! [`AvalonClient`], never [`crate::Session`]/[`crate::AccountSession`] —
//! the entire premise of recovery is that the caller has no valid session
//! for the identity being recovered, so `start_request`/`finish_request`
//! are (deliberately, same as server-side) the one exception to this
//! crate's usual "every write needs a session" shape. `get_request`/
//! `identity_recovery_status`/`finalize_request` are public reads/writes
//! too, matching the server's own "the fact of an in-flight recovery must
//! be checkable by anyone" invariant.
//!
//! **Same virtual-authenticator ceremony as account registration.**
//! [`AvalonClient::start_recovery_request`] drives a real WebAuthn
//! registration ceremony against the challenge `POST
//! /recovery/requests/start` returns, using the exact same
//! `account::webauthn` driver `AvalonClient::register` already proves
//! against a live server — this crate's native/CLI context has no real
//! browser WebAuthn surface, so both flows go through the same
//! software-only virtual authenticator.

use uuid::Uuid;

use crate::account::webauthn;
use crate::{AvalonClient, SdkError};

/// A recovery request in progress or resolved — mirrors
/// `crates/server/src/recovery.rs::RecoveryRequestResponse` at the wire
/// level.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RecoveryRequest {
    /// This request's own id.
    pub id: Uuid,
    /// The identity being recovered.
    pub identity_id: Uuid,
    /// `"pending_approvals"`, `"delay"`, `"completed"`, `"cancelled"`, or
    /// `"expired"` — `crates/server/src/recovery.rs`'s own stable
    /// vocabulary.
    pub status: String,
    /// Approvals required before finalization is possible.
    pub threshold: i32,
    /// Approvals received so far.
    pub approvals_count: i64,
    /// When this request was started.
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: time::OffsetDateTime,
    /// The mandatory public delay's end, once the threshold is met.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub delay_ends_at: Option<time::OffsetDateTime>,
}

#[derive(serde::Serialize)]
struct StartRequestWire<'a> {
    identity_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_label: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct StartRequestResponse {
    ticket_id: Uuid,
    challenge: passkey_types::webauthn::CredentialCreationOptions,
}

#[derive(serde::Serialize)]
struct FinishRequestWire {
    ticket_id: Uuid,
    webauthn_credential: passkey_types::webauthn::CreatedPublicKeyCredential,
}

impl AvalonClient {
    /// `POST /recovery/requests/start` then a real WebAuthn registration
    /// ceremony then `POST /recovery/requests/finish` (issue #201, closed
    /// out by #741/#747) — begins recovering `identity_id` with a
    /// brand-new device, all in one call. Unauthenticated by necessity
    /// (see module doc comment); refused with the same
    /// [`SdkError::Rejected`] for "this identity doesn't exist" and "this
    /// identity has no guardians configured," so this method can never be
    /// used to distinguish the two. `device_label` is stored only for
    /// display once recovery completes.
    pub async fn start_recovery_request(
        &self,
        identity_id: Uuid,
        device_label: Option<&str>,
    ) -> Result<RecoveryRequest, SdkError> {
        let start_response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!(
                "{}/recovery/requests/start",
                self.config.server_url
            ))
            .json(&StartRequestWire {
                identity_id,
                device_label,
            })
        })
        .await?;
        if !start_response.status().is_success() {
            return Err(crate::http::map_error_response(start_response).await);
        }
        let start: StartRequestResponse = start_response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        let (webauthn_credential, _stored) = webauthn::registration_ceremony(start.challenge)
            .await
            .map_err(|_| {
                SdkError::Protocol("recovery WebAuthn registration ceremony failed".to_string())
            })?;

        // Sent directly rather than through `crate::http::send`'s retry
        // wrapper — same posture `account::AvalonClient::register`'s own
        // finish call takes: `ticket_id` is a single-use ceremony ticket,
        // so retrying this exchange wouldn't actually retry anything, it
        // would just fail differently against an already-consumed ticket.
        let finish_response = self
            .http
            .post(format!(
                "{}/recovery/requests/finish",
                self.config.server_url
            ))
            .json(&FinishRequestWire {
                ticket_id: start.ticket_id,
                webauthn_credential,
            })
            .send()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        if !finish_response.status().is_success() {
            return Err(crate::http::map_error_response(finish_response).await);
        }
        finish_response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    /// `GET /recovery/requests/{id}` (closed out by #741/#747) — public,
    /// deliberately: the mandatory public delay means the fact of an
    /// in-flight recovery, and when its delay ends, must be checkable by
    /// anyone, never just the owner or its guardians. Never exposes which
    /// specific guardians have approved, only the count.
    pub async fn get_recovery_request(
        &self,
        request_id: Uuid,
    ) -> Result<RecoveryRequest, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/recovery/requests/{request_id}",
                self.config.server_url
            ))
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

    /// `GET /identities/{id}/recovery/status` (closed out by #741/#747) —
    /// the same public data as [`AvalonClient::get_recovery_request`], but
    /// keyed by identity rather than request id, for a caller who doesn't
    /// already know a request id. `None` means no active recovery.
    pub async fn identity_recovery_status(
        &self,
        identity_id: Uuid,
    ) -> Result<Option<RecoveryRequest>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/identities/{identity_id}/recovery/status",
                self.config.server_url
            ))
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

    /// `POST /recovery/requests/{id}/finalize` (closed out by #741/#747)
    /// — deliberately public and idempotent: it grants nothing beyond
    /// what guardian approval plus the elapsed public delay already
    /// durably authorized, so there is no meaningful caller identity to
    /// check. Calling it on an already-completed request simply returns
    /// the current state rather than erroring.
    pub async fn finalize_recovery_request(
        &self,
        request_id: Uuid,
    ) -> Result<RecoveryRequest, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!(
                "{}/recovery/requests/{request_id}/finalize",
                self.config.server_url
            ))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_request_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "id": Uuid::nil(),
            "identity_id": Uuid::nil(),
            "status": "pending_approvals",
            "threshold": 2,
            "approvals_count": 1,
            "requested_at": "2026-01-01T00:00:00Z",
            "delay_ends_at": null,
        });
        let request: RecoveryRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(request.status, "pending_approvals");
        assert_eq!(request.threshold, 2);
        assert_eq!(request.approvals_count, 1);
        assert!(request.delay_ends_at.is_none());
    }
}
