//! Social recovery (issue #201/#443) on [`super::AccountSession`] — M-of-N
//! guardian-based recovery when every passkey is lost. See
//! `crates/server/src/recovery.rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::{AccountSession, SignatureFields};

/// The caller's own guardian configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct GuardianSettings {
    /// Currently-named guardians.
    pub guardian_ids: Vec<Uuid>,
    /// How many guardian approvals a recovery attempt needs.
    pub threshold: i32,
    /// When this configuration was last changed, if ever.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

/// A recovery attempt in progress or resolved.
#[derive(Debug, Clone, Deserialize)]
pub struct RecoveryRequest {
    /// This request's own id.
    pub id: Uuid,
    /// The identity being recovered.
    pub identity_id: Uuid,
    /// `"pending"`, `"approved"`, `"completed"`, `"cancelled"`, or
    /// `"expired"` — `crates/server/src/recovery.rs`'s own stable
    /// vocabulary.
    pub status: String,
    /// Approvals required before finalization is possible.
    pub threshold: i32,
    /// Approvals received so far.
    pub approvals_count: i64,
    /// When this request was started.
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
    /// The mandatory public delay's end, once the threshold is met.
    #[serde(with = "time::serde::rfc3339::option")]
    pub delay_ends_at: Option<OffsetDateTime>,
}

/// One active recovery request where the caller is currently a guardian.
#[derive(Debug, Clone, Deserialize)]
pub struct GuardianRequest {
    /// The request itself.
    pub request: RecoveryRequest,
    /// Whether the caller has already approved this one.
    pub already_approved: bool,
}

/// An identity currently relying on the caller as a recovery guardian.
#[derive(Debug, Clone, Deserialize)]
pub struct GuardianOf {
    /// The relying identity's id.
    pub identity_id: Uuid,
    /// The relying identity's display name.
    pub display_name: String,
    /// When the caller was named as a guardian for this identity.
    #[serde(with = "time::serde::rfc3339")]
    pub added_at: OffsetDateTime,
}

#[derive(Serialize)]
struct SetGuardiansRequest {
    guardian_ids: Vec<Uuid>,
    threshold: i32,
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize)]
struct CancelRecoveryRequest<'a> {
    reason: Option<&'a str>,
}

impl AccountSession {
    /// `GET /me/recovery/guardians`.
    pub async fn guardians(&self) -> Result<GuardianSettings, SdkError> {
        self.get("/me/recovery/guardians").await
    }

    /// `PUT /me/recovery/guardians` — always signs
    /// (`recovery.guardians.set`, `[identity_id, sorted guardian ids
    /// comma-joined, threshold]`), whether or not this call actually
    /// removes a guardian or raises the threshold (the only cases #697
    /// requires it for) — same "unused-but-valid signature is harmless"
    /// simplification every other conditionally-signed method in this
    /// crate makes.
    pub async fn set_guardians(
        &self,
        guardian_ids: &[Uuid],
        threshold: i32,
    ) -> Result<GuardianSettings, SdkError> {
        let mut sorted: Vec<String> = guardian_ids.iter().map(Uuid::to_string).collect();
        sorted.sort();
        let signature = self.sign(
            "recovery.guardians.set",
            &[
                &self.identity().id.0.to_string(),
                &sorted.join(","),
                &threshold.to_string(),
            ],
        );
        self.put(
            "/me/recovery/guardians",
            &SetGuardiansRequest {
                guardian_ids: guardian_ids.to_vec(),
                threshold,
                signature,
            },
        )
        .await
    }

    /// `GET /me/recovery/status` — the caller's own in-flight recovery
    /// request, if any.
    pub async fn my_recovery_status(&self) -> Result<Option<RecoveryRequest>, SdkError> {
        self.get("/me/recovery/status").await
    }

    /// `GET /me/recovery/guardian-requests` — every active request where
    /// the caller is currently a guardian.
    pub async fn guardian_requests(&self) -> Result<Vec<GuardianRequest>, SdkError> {
        self.get("/me/recovery/guardian-requests").await
    }

    /// `GET /me/recovery/guardian-of` — every identity currently relying on
    /// the caller as a guardian.
    pub async fn guardian_of(&self) -> Result<Vec<GuardianOf>, SdkError> {
        self.get("/me/recovery/guardian-of").await
    }

    /// `DELETE /me/recovery/guardian-of/{identity_id}` — the caller
    /// self-removing as someone else's guardian. Not signature-required
    /// (self-removal only narrows a guardian assignment).
    pub async fn resign_as_guardian(&self, identity_id: Uuid) -> Result<(), SdkError> {
        self.delete(&format!("/me/recovery/guardian-of/{identity_id}"))
            .await
    }

    /// `POST /recovery/requests/{id}/approve` — a guardian approving
    /// someone else's in-flight recovery request.
    pub async fn approve_recovery_request(
        &self,
        request_id: Uuid,
    ) -> Result<RecoveryRequest, SdkError> {
        self.post_empty(&format!("/recovery/requests/{request_id}/approve"))
            .await
    }

    /// `POST /recovery/requests/{id}/cancel` — the veto path: either the
    /// identity's own owner or any current guardian.
    pub async fn cancel_recovery_request(
        &self,
        request_id: Uuid,
        reason: Option<&str>,
    ) -> Result<RecoveryRequest, SdkError> {
        self.post(
            &format!("/recovery/requests/{request_id}/cancel"),
            &CancelRecoveryRequest { reason },
        )
        .await
    }
}
