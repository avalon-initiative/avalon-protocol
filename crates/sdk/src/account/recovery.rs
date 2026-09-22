//! Social recovery (issue #201/#443) on [`super::AccountSession`] — M-of-N
//! guardian-based recovery when every passkey is lost. See
//! `crates/server/src/recovery.rs`.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::AccountSession;

/// The caller's own guardian configuration.
#[derive(Debug, Clone)]
pub struct GuardianSettings {
    /// Currently-named guardians.
    pub guardian_ids: Vec<Uuid>,
    /// How many guardian approvals a recovery attempt needs.
    pub threshold: i32,
    /// When this configuration was last changed, if ever.
    pub updated_at: Option<OffsetDateTime>,
}

impl TryFrom<crate::generated::GuardianSettingsResponse> for GuardianSettings {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuardianSettingsResponse) -> Result<Self, SdkError> {
        Ok(GuardianSettings {
            guardian_ids: body.guardian_ids,
            threshold: body.threshold,
            updated_at: body
                .updated_at
                .as_deref()
                .map(super::parse_rfc3339)
                .transpose()?,
        })
    }
}

/// A recovery attempt in progress or resolved.
#[derive(Debug, Clone)]
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
    pub requested_at: OffsetDateTime,
    /// The mandatory public delay's end, once the threshold is met.
    pub delay_ends_at: Option<OffsetDateTime>,
}

impl TryFrom<crate::generated::RecoveryRequestResponse> for RecoveryRequest {
    type Error = SdkError;

    fn try_from(body: crate::generated::RecoveryRequestResponse) -> Result<Self, SdkError> {
        Ok(RecoveryRequest {
            id: body.id,
            identity_id: body.identity_id,
            status: body.status,
            threshold: body.threshold,
            approvals_count: body.approvals_count,
            requested_at: super::parse_rfc3339(&body.requested_at)?,
            delay_ends_at: body
                .delay_ends_at
                .as_deref()
                .map(super::parse_rfc3339)
                .transpose()?,
        })
    }
}

/// One active recovery request where the caller is currently a guardian.
#[derive(Debug, Clone)]
pub struct GuardianRequest {
    /// The request itself.
    pub request: RecoveryRequest,
    /// Whether the caller has already approved this one.
    pub already_approved: bool,
}

impl TryFrom<crate::generated::GuardianRequestSummary> for GuardianRequest {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuardianRequestSummary) -> Result<Self, SdkError> {
        Ok(GuardianRequest {
            request: body.request.try_into()?,
            already_approved: body.already_approved,
        })
    }
}

/// An identity currently relying on the caller as a recovery guardian.
#[derive(Debug, Clone)]
pub struct GuardianOf {
    /// The relying identity's id.
    pub identity_id: Uuid,
    /// The relying identity's display name.
    pub display_name: String,
    /// When the caller was named as a guardian for this identity.
    pub added_at: OffsetDateTime,
}

impl TryFrom<crate::generated::GuardianOfSummary> for GuardianOf {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuardianOfSummary) -> Result<Self, SdkError> {
        Ok(GuardianOf {
            identity_id: body.identity_id,
            display_name: body.display_name,
            added_at: super::parse_rfc3339(&body.added_at)?,
        })
    }
}

impl AccountSession {
    /// `GET /me/recovery/guardians`.
    pub async fn guardians(&self) -> Result<GuardianSettings, SdkError> {
        let raw: crate::generated::GuardianSettingsResponse = self
            .get(crate::generated::paths::recovery::GET_GUARDIANS)
            .await?;
        raw.try_into()
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
        let raw: crate::generated::GuardianSettingsResponse = self
            .put(
                crate::generated::paths::recovery::SET_GUARDIANS,
                &crate::generated::SetGuardiansRequest {
                    guardian_ids: guardian_ids.to_vec(),
                    threshold,
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /me/recovery/status` — the caller's own in-flight recovery
    /// request, if any.
    pub async fn my_recovery_status(&self) -> Result<Option<RecoveryRequest>, SdkError> {
        let raw: Option<crate::generated::RecoveryRequestResponse> = self
            .get(crate::generated::paths::recovery::MY_RECOVERY_STATUS)
            .await?;
        raw.map(RecoveryRequest::try_from).transpose()
    }

    /// `GET /me/recovery/guardian-requests` — every active request where
    /// the caller is currently a guardian.
    pub async fn guardian_requests(&self) -> Result<Vec<GuardianRequest>, SdkError> {
        let raw: Vec<crate::generated::GuardianRequestSummary> = self
            .get(crate::generated::paths::recovery::GUARDIAN_REQUESTS)
            .await?;
        raw.into_iter().map(GuardianRequest::try_from).collect()
    }

    /// `GET /me/recovery/guardian-of` — every identity currently relying on
    /// the caller as a guardian.
    pub async fn guardian_of(&self) -> Result<Vec<GuardianOf>, SdkError> {
        let raw: Vec<crate::generated::GuardianOfSummary> = self
            .get(crate::generated::paths::recovery::GUARDIAN_OF)
            .await?;
        raw.into_iter().map(GuardianOf::try_from).collect()
    }

    /// `DELETE /me/recovery/guardian-of/{identity_id}` — the caller
    /// self-removing as someone else's guardian. Not signature-required
    /// (self-removal only narrows a guardian assignment).
    pub async fn resign_as_guardian(&self, identity_id: Uuid) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::recovery::RESIGN_GUARDIAN,
            &[("identity_id", &identity_id.to_string())],
        ))
        .await
    }

    /// `POST /recovery/requests/{id}/approve` — a guardian approving
    /// someone else's in-flight recovery request.
    pub async fn approve_recovery_request(
        &self,
        request_id: Uuid,
    ) -> Result<RecoveryRequest, SdkError> {
        let raw: crate::generated::RecoveryRequestResponse = self
            .post_empty(&super::path(
                crate::generated::paths::recovery::APPROVE_REQUEST,
                &[("id", &request_id.to_string())],
            ))
            .await?;
        raw.try_into()
    }

    /// `POST /recovery/requests/{id}/cancel` — the veto path: either the
    /// identity's own owner or any current guardian.
    pub async fn cancel_recovery_request(
        &self,
        request_id: Uuid,
        reason: Option<&str>,
    ) -> Result<RecoveryRequest, SdkError> {
        let raw: crate::generated::RecoveryRequestResponse = self
            .post(
                &super::path(
                    crate::generated::paths::recovery::CANCEL_REQUEST,
                    &[("id", &request_id.to_string())],
                ),
                &crate::generated::CancelRecoveryRequest {
                    reason: reason.map(str::to_string),
                },
            )
            .await?;
        raw.try_into()
    }
}
