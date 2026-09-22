//! Integrator connect/consent (issue #27/#83) on [`super::AccountSession`]
//! — an identity granting or revoking its own consent to an integrator, not
//! anything the integrator does on its own behalf. See
//! `crates/server/src/connections.rs`.

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::{AccountSession, SignatureFields};

/// The result of a successful `connect` call.
#[derive(Debug, Clone)]
pub struct IntegratorConnection {
    /// This binding's own id.
    pub binding_id: Uuid,
    /// The integrator connected to.
    pub integrator_id: Uuid,
    /// When the binding was established.
    pub established_at: OffsetDateTime,
    /// Every capability actually granted (may be a subset of what was
    /// requested).
    pub granted_capabilities: Vec<String>,
}

impl TryFrom<crate::generated::ConnectResponse> for IntegratorConnection {
    type Error = SdkError;

    fn try_from(body: crate::generated::ConnectResponse) -> Result<Self, SdkError> {
        Ok(IntegratorConnection {
            binding_id: body.binding_id,
            integrator_id: body.integrator_id,
            established_at: super::parse_rfc3339(&body.established_at)?,
            granted_capabilities: body.granted_capabilities,
        })
    }
}

/// One currently-active grant within a [`MyConnection`].
#[derive(Debug, Clone)]
pub struct ConnectionGrant {
    /// The granted capability.
    pub capability: String,
    /// When it was granted.
    pub granted_at: OffsetDateTime,
}

impl TryFrom<crate::generated::ConnectionGrant> for ConnectionGrant {
    type Error = SdkError;

    fn try_from(body: crate::generated::ConnectionGrant) -> Result<Self, SdkError> {
        Ok(ConnectionGrant {
            capability: body.capability,
            granted_at: super::parse_rfc3339(&body.granted_at)?,
        })
    }
}

/// One of the caller's own active integrator connections (bindings).
#[derive(Debug, Clone)]
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
    pub established_at: OffsetDateTime,
    /// Every currently-active grant.
    pub grants: Vec<ConnectionGrant>,
}

impl TryFrom<crate::generated::Connection> for MyConnection {
    type Error = SdkError;

    fn try_from(body: crate::generated::Connection) -> Result<Self, SdkError> {
        Ok(MyConnection {
            binding_id: body.binding_id,
            integrator_id: body.integrator_id,
            slug: body.slug,
            name: body.name,
            established_at: super::parse_rfc3339(&body.established_at)?,
            grants: body
                .grants
                .into_iter()
                .map(ConnectionGrant::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
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
        let raw: crate::generated::ConnectResponse = self
            .post(
                &super::path(
                    crate::generated::paths::integrators::CONNECT,
                    &[("slug", slug)],
                ),
                &ConnectRequest {
                    capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
                    signature,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `DELETE /integrations/{slug}/connect`. Not signature-required
    /// (revocation only narrows what an integrator can do).
    pub async fn disconnect_integrator(&self, slug: &str) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::integrators::DISCONNECT,
            &[("slug", slug)],
        ))
        .await
    }

    /// `DELETE /integrations/{slug}/grants/{capability}`. Not
    /// signature-required, same reasoning as `disconnect_integrator`.
    pub async fn revoke_grant(&self, slug: &str, capability: &str) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::integrators::REVOKE_GRANT,
            &[("slug", slug), ("capability", capability)],
        ))
        .await
    }

    /// `GET /me/connections` — every integrator this identity has
    /// currently consented to, and what it granted each one.
    pub async fn my_connections(&self) -> Result<Vec<MyConnection>, SdkError> {
        let raw: Vec<crate::generated::Connection> = self
            .get(crate::generated::paths::integrators::LIST_MY_CONNECTIONS)
            .await?;
        raw.into_iter().map(MyConnection::try_from).collect()
    }
}
