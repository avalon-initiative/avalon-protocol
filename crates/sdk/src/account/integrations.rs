//! Integrator connect/consent (issue #27/#83) on [`super::AccountSession`]
//! — an identity granting or revoking its own consent to an integrator, not
//! anything the integrator does on its own behalf. See
//! `crates/server/src/connections.rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::{AccountSession, SignatureFields};

/// The result of a successful `connect` call.
#[derive(Debug, Clone, Deserialize)]
pub struct IntegratorConnection {
    /// This binding's own id.
    pub binding_id: Uuid,
    /// The integrator connected to.
    pub integrator_id: Uuid,
    /// When the binding was established.
    #[serde(with = "time::serde::rfc3339")]
    pub established_at: OffsetDateTime,
    /// Every capability actually granted (may be a subset of what was
    /// requested).
    pub granted_capabilities: Vec<String>,
}

/// One currently-active grant within a [`MyConnection`].
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionGrant {
    /// The granted capability.
    pub capability: String,
    /// When it was granted.
    #[serde(with = "time::serde::rfc3339")]
    pub granted_at: OffsetDateTime,
}

/// One of the caller's own active integrator connections (bindings).
#[derive(Debug, Clone, Deserialize)]
pub struct MyConnection {
    /// This binding's own id.
    pub binding_id: Uuid,
    /// The connected integrator's id.
    pub integrator_id: Uuid,
    /// The connected integrator's slug.
    pub slug: String,
    /// The connected integrator's display name.
    pub name: String,
    /// When the binding was established.
    #[serde(with = "time::serde::rfc3339")]
    pub established_at: OffsetDateTime,
    /// Every currently-active grant.
    pub grants: Vec<ConnectionGrant>,
}

#[derive(Serialize)]
struct ConnectRequest {
    capabilities: Vec<String>,
    #[serde(flatten)]
    signature: SignatureFields,
}

impl AccountSession {
    /// `POST /integrations/{slug}/connect` — the one call that hands a
    /// third party standing permission over this identity's data going
    /// forward; always signed (`integration.connect`,
    /// `[slug, capabilities comma-joined in request order]`). Idempotent:
    /// reconnecting to an already-bound integrator doesn't duplicate the
    /// binding, but does still grant any newly-approved capabilities.
    pub async fn connect_integrator(
        &self,
        slug: &str,
        capabilities: &[&str],
    ) -> Result<IntegratorConnection, SdkError> {
        let joined = capabilities.join(",");
        let signature = self.sign("integration.connect", &[slug, &joined]);
        self.post(
            &format!("/integrations/{slug}/connect"),
            &ConnectRequest {
                capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
                signature,
            },
        )
        .await
    }

    /// `DELETE /integrations/{slug}/connect`. Not signature-required
    /// (revocation only narrows what an integrator can do).
    pub async fn disconnect_integrator(&self, slug: &str) -> Result<(), SdkError> {
        self.delete(&format!("/integrations/{slug}/connect")).await
    }

    /// `DELETE /integrations/{slug}/grants/{capability}`. Not
    /// signature-required, same reasoning as `disconnect_integrator`.
    pub async fn revoke_grant(&self, slug: &str, capability: &str) -> Result<(), SdkError> {
        self.delete(&format!("/integrations/{slug}/grants/{capability}"))
            .await
    }

    /// `GET /me/connections` — every integrator this identity has
    /// currently consented to, and what it granted each one.
    pub async fn my_connections(&self) -> Result<Vec<MyConnection>, SdkError> {
        self.get("/me/connections").await
    }
}
