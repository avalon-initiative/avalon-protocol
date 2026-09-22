//! Device-registration / linked-device grant model (issue #135) and
//! cross-device pairing approval (issue #307/#704) on
//! [`super::AccountSession`] — `identity_signing_keys` rows (event-authorship
//! keys), distinct from `passkeys` (WebAuthn login credentials). See
//! `crates/server/src/devices.rs`/`device_pairing.rs`.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::{canonical_message, AccountSession};

/// One of this identity's registered signing-key devices.
#[derive(Debug, Clone)]
pub struct Device {
    /// The `identity_signing_keys.id` this device is stored under — the
    /// value a signature-required action's `signing_key_id` field names.
    pub id: Uuid,
    /// User-chosen label, if any.
    pub label: Option<String>,
    /// Base64-encoded raw Ed25519 public key.
    pub public_key: String,
    /// When this device's key was registered.
    pub added_at: OffsetDateTime,
    /// `None` while active; set once revoked.
    pub revoked_at: Option<OffsetDateTime>,
}

impl TryFrom<crate::generated::DeviceResponse> for Device {
    type Error = SdkError;

    fn try_from(body: crate::generated::DeviceResponse) -> Result<Self, SdkError> {
        Ok(Device {
            id: body.id,
            label: body.label,
            public_key: body.public_key,
            added_at: super::parse_rfc3339(&body.added_at)?,
            revoked_at: body
                .revoked_at
                .as_deref()
                .map(super::parse_rfc3339)
                .transpose()?,
        })
    }
}

/// A pending or resolved request to add a new device's signing key —
/// `POST /me/devices/grants`.
#[derive(Debug, Clone)]
pub struct DeviceGrant {
    /// This grant's own id.
    pub id: Uuid,
    /// `"pending"`, `"approved"`, `"denied"`, or `"expired"` —
    /// `crates/server/src/devices.rs`'s own stable vocabulary, not
    /// re-modeled as an enum here (see that module for the authoritative
    /// list).
    pub status: String,
    /// The requesting device's own chosen label, if any.
    pub device_label: Option<String>,
    /// Base64-encoded — the exact bytes an approving device must include
    /// in what it signs (see [`AccountSession::approve_device_grant`]).
    pub requested_signing_public_key: String,
    /// When this grant was requested.
    pub requested_at: OffsetDateTime,
    /// When this grant expires if never approved/denied.
    pub expires_at: OffsetDateTime,
}

impl TryFrom<crate::generated::DeviceGrantResponse> for DeviceGrant {
    type Error = SdkError;

    fn try_from(body: crate::generated::DeviceGrantResponse) -> Result<Self, SdkError> {
        Ok(DeviceGrant {
            id: body.id,
            status: body.status,
            device_label: body.device_label,
            requested_signing_public_key: body.requested_signing_public_key,
            requested_at: super::parse_rfc3339(&body.requested_at)?,
            expires_at: super::parse_rfc3339(&body.expires_at)?,
        })
    }
}

/// Exact bytes `devices::device_grant_approval_signing_bytes` on the
/// server reconstructs — must match
/// `packages/api-client/src/crypto/signingKey.ts::deviceGrantApprovalSigningBytes`
/// byte-for-byte. Shares [`canonical_message`]'s
/// `avalon:<tag>:v1:<field>:...` shape (`device_grant.approved` as the
/// tag), even though this predates #698's generalized signature-gate
/// module.
fn device_grant_approval_signing_bytes(
    grant_id: Uuid,
    identity_id: Uuid,
    requested_signing_public_key_b64: &str,
) -> Vec<u8> {
    canonical_message(
        "device_grant.approved",
        &[
            &grant_id.to_string(),
            &identity_id.to_string(),
            requested_signing_public_key_b64,
        ],
    )
}

impl AccountSession {
    /// `GET /me/devices` — every signing-key device registered to this
    /// identity, active and revoked alike.
    pub async fn list_devices(&self) -> Result<Vec<Device>, SdkError> {
        let raw: Vec<crate::generated::DeviceResponse> = self.get("/me/devices").await?;
        raw.into_iter().map(Device::try_from).collect()
    }

    /// `PATCH /me/devices/{signing_key_id}` — relabels a device. Not
    /// signature-required.
    pub async fn rename_device(
        &self,
        signing_key_id: Uuid,
        label: &str,
    ) -> Result<Device, SdkError> {
        let raw: crate::generated::DeviceResponse = self
            .patch(
                &format!("/me/devices/{signing_key_id}"),
                &crate::generated::RenameDeviceRequest {
                    label: label.to_string(),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /me/devices/{signing_key_id}/revoke` — unilateral, ambient-token
    /// (revocation only ever narrows trust, per #697).
    pub async fn revoke_device(&self, signing_key_id: Uuid) -> Result<(), SdkError> {
        self.post_empty_no_response(&format!("/me/devices/{signing_key_id}/revoke"))
            .await
    }

    /// `POST /me/devices/grants` — requests a new device's signing key be
    /// added, from the *requesting* device's own session (which has no
    /// signing key of its own yet — that's the whole point). Requesting
    /// confers no access by itself; see
    /// [`AccountSession::approve_device_grant`].
    pub async fn request_device_grant(
        &self,
        requested_signing_public_key_b64: &str,
        device_label: Option<&str>,
    ) -> Result<DeviceGrant, SdkError> {
        let raw: crate::generated::DeviceGrantResponse = self
            .post(
                "/me/devices/grants",
                &crate::generated::RequestDeviceGrantRequest {
                    requested_signing_public_key: requested_signing_public_key_b64.to_string(),
                    device_label: device_label.map(str::to_string),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /me/devices/grants[?status=]`.
    pub async fn list_device_grants(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<DeviceGrant>, SdkError> {
        let raw: Vec<crate::generated::DeviceGrantResponse> = match status {
            Some(status) => {
                self.get_query("/me/devices/grants", &[("status", status)])
                    .await?
            }
            None => self.get("/me/devices/grants").await?,
        };
        raw.into_iter().map(DeviceGrant::try_from).collect()
    }

    /// `GET /me/devices/grants/{id}`.
    pub async fn get_device_grant(&self, grant_id: Uuid) -> Result<DeviceGrant, SdkError> {
        let raw: crate::generated::DeviceGrantResponse =
            self.get(&format!("/me/devices/grants/{grant_id}")).await?;
        raw.try_into()
    }

    /// `POST /me/devices/grants/{id}/approve` — approves someone else's (or
    /// this identity's own, from a different device's) pending grant,
    /// signed with this session's own local key over
    /// `device_grant_approval_signing_bytes(grant_id, identity_id,
    /// requested_signing_public_key)` — proving the approval came from a
    /// device that once passed a real WebAuthn ceremony. Returns
    /// [`SdkError::MissingIssuerCredentials`]-shaped failure via
    /// [`SdkError::Rejected`] server-side if this session has no local
    /// signing key at all (see [`AccountSession::signing_key_id`]).
    pub async fn approve_device_grant(
        &self,
        grant_id: Uuid,
        requested_signing_public_key_b64: &str,
    ) -> Result<Device, SdkError> {
        let signing = self.signing_key_id().ok_or_else(|| {
            SdkError::Protocol(
                "approve_device_grant requires a local signing key — this AccountSession has none"
                    .to_string(),
            )
        })?;
        let message = device_grant_approval_signing_bytes(
            grant_id,
            self.identity().id.0,
            requested_signing_public_key_b64,
        );
        // Reuses `sign`'s own key rather than re-deriving — `sign` always
        // uses `canonical_message`, whose output for tag
        // `device_grant.approved` is exactly `message` above, so this
        // calls the signer directly instead of going through `sign` a
        // second time with a slightly different call shape.
        let signature = self.sign_raw(&message);
        let raw: crate::generated::DeviceResponse = self
            .post(
                &format!("/me/devices/grants/{grant_id}/approve"),
                &crate::generated::ApproveDeviceGrantRequest {
                    approver_signing_key_id: signing,
                    signature,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /auth/device/approve` (issue #307/#704) — approves a
    /// cross-device pairing request identified by `user_code`, always
    /// signed (`device_pairing.approve`, `[identity_id, user_code]`).
    pub async fn approve_device_pairing(&self, user_code: &str) -> Result<String, SdkError> {
        let signature = self.sign(
            "device_pairing.approve",
            &[&self.identity().id.0.to_string(), user_code],
        );
        let response: crate::generated::ResolvePairingResponse = self
            .post(
                "/auth/device/approve",
                &crate::generated::ApprovePairingRequest {
                    user_code: user_code.to_string(),
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        Ok(response.status)
    }

    /// `POST /auth/device/deny` — declines a pairing request. Not
    /// signature-required (no state is granted).
    pub async fn deny_device_pairing(&self, user_code: &str) -> Result<String, SdkError> {
        let response: crate::generated::ResolvePairingResponse = self
            .post(
                "/auth/device/deny",
                &crate::generated::UserCodeRequest {
                    user_code: user_code.to_string(),
                },
            )
            .await?;
        Ok(response.status)
    }
}
