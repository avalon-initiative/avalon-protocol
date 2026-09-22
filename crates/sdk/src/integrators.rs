//! Integrator registration and issuer-key management (issue #741/#746) —
//! previously an integrator/game developer had no SDK-level way to
//! register their integration or manage its issuer keys, only whatever
//! `avalon-cli`'s dev tooling or a direct HTTP call provided. See
//! `crates/server/src/integrators.rs` for the real request/response
//! contracts every method here mirrors.
//!
//! Every method lives on [`AvalonClient`], not [`crate::Session`]:
//! registering an integrator happens *before* any credential exists to
//! build a [`crate::Session`] from, and managing an already-registered
//! integrator's own keys (`add_issuer_key`/`revoke_issuer_key`/
//! `integrator_whoami`) needs only this integrator's own challenge-response
//! proof (`AvalonConfig::integrator_slug`/`signing_key`) — the same
//! "integrator-credentialed call, no user session required" shape
//! [`crate::issuer_registration::AvalonClient::register_issuer`] already
//! establishes, rather than [`crate::achievements`]/[`crate::schema`]'s
//! convention of hanging integrator-credentialed writes off [`crate::Session`].
//! `list_integrators`/`get_integrator`/`list_issuer_keys` are public,
//! unauthenticated reads, same visibility level `registry.rs`/`schema.rs`'s
//! own public reads already use.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{AvalonClient, SdkError};

#[derive(Deserialize)]
struct ChallengeResponse {
    challenge_id: Uuid,
    nonce: String,
}

/// This integrator's initial registered key — mirrors
/// `crates/server/src/integrators.rs::InitialKeyRequest`.
#[derive(Debug, Clone)]
pub struct InitialKey {
    /// The only algorithm registration currently accepts: `"ed25519"`.
    pub algorithm: String,
    /// Standard-base64-encoded raw public key bytes.
    pub public_key: String,
}

#[derive(Serialize)]
struct InitialKeyWire<'a> {
    algorithm: &'a str,
    public_key: &'a str,
}

/// The fields [`AvalonClient::register_integrator`] needs — mirrors
/// `crates/server/src/integrators.rs::CreateIntegratorRequest`.
#[derive(Debug, Clone)]
pub struct NewIntegrator {
    /// Lowercase `[a-z0-9-]`, this integrator's own permanent slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// The registering developer/organization's own display name.
    pub owner_name: String,
    /// Capabilities this integrator intends to request grants for —
    /// declarative only, never itself a grant (issue #27 owns those).
    pub requested_capabilities: Vec<String>,
    /// This integrator's first key — doubles as its root key and its
    /// first operational key (#80/#84).
    pub initial_key: InitialKey,
    /// `"game"` (the default if omitted), `"app"`, or `"service"` (#282)
    /// — fixed for this integrator's lifetime, and what decides whether it
    /// can later define achievements (`Game` only) or milestones (`App`/
    /// `Service` only, #324).
    pub category: Option<String>,
}

#[derive(Serialize)]
struct CreateIntegratorRequest<'a> {
    slug: &'a str,
    name: &'a str,
    owner_name: &'a str,
    requested_capabilities: &'a [String],
    initial_key: InitialKeyWire<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<&'a str>,
}

/// The one-time credential [`AvalonClient::register_integrator`] returns —
/// mirrors `crates/server/src/integrators.rs::IntegratorCredentialResponse`.
/// Nothing else in this SDK ever returns `key_id` again except
/// [`AvalonClient::list_issuer_keys`]'s public (non-secret) view.
#[derive(Debug, Clone, Deserialize)]
pub struct IntegratorCredential {
    /// This integrator's own id.
    pub integrator_id: Uuid,
    /// The registered initial key's own id — feed this into
    /// `AvalonConfig::integrator_credential_key_id` for subsequent calls.
    pub key_id: String,
}

/// The full registration response — mirrors
/// `crates/server/src/integrators.rs::IntegratorResponse`. Returned only
/// once, to the registrant, at registration time; every later read goes
/// through [`IntegratorPublic`] instead, which drops [`Self::credential`].
#[derive(Debug, Clone, Deserialize)]
pub struct Integrator {
    /// This integrator's own id.
    pub id: Uuid,
    /// This integrator's own permanent slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// The registering developer/organization's own display name.
    pub owner_name: String,
    /// When this integrator registered.
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    /// This integrator's own status (e.g. `"active"`).
    pub status: String,
    /// `"game"`, `"app"`, or `"service"`.
    pub category: String,
    /// Capabilities this integrator declared it intends to request.
    pub requested_capabilities: Vec<String>,
    /// The credential this registration minted — see that field's own doc
    /// comment on when to persist it.
    pub credential: IntegratorCredential,
}

/// A public read of an integrator's registration — mirrors
/// `crates/server/src/integrators.rs::IntegratorPublicResponse`. No
/// credential fields, unlike [`Integrator`] (which only registration
/// itself ever returns).
#[derive(Debug, Clone, Deserialize)]
pub struct IntegratorPublic {
    /// This integrator's own id.
    pub id: Uuid,
    /// This integrator's own permanent slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// The registering developer/organization's own display name.
    pub owner_name: String,
    /// When this integrator registered.
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    /// This integrator's own status (e.g. `"active"`).
    pub status: String,
    /// `"game"`, `"app"`, or `"service"`.
    pub category: String,
    /// Capabilities this integrator declared it intends to request.
    pub requested_capabilities: Vec<String>,
}

/// One entry in `GET /integrations`'s results — mirrors
/// `crates/server/src/integrators.rs::IntegratorSummary`.
#[derive(Debug, Clone, Deserialize)]
pub struct IntegratorSummary {
    /// This integrator's own id.
    pub id: Uuid,
    /// This integrator's own permanent slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// The registering developer/organization's own display name.
    pub owner_name: String,
    /// When this integrator registered.
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    /// This integrator's own status (e.g. `"active"`).
    pub status: String,
    /// `"game"`, `"app"`, or `"service"`.
    pub category: String,
}

#[derive(Deserialize)]
struct ListIntegratorsResponseWire {
    integrators: Vec<IntegratorSummary>,
    #[allow(dead_code)]
    next_cursor: Option<Uuid>,
}

/// `sort=` values [`AvalonClient::list_integrators`] accepts — mirrors
/// `crates/server/src/integrators.rs::IntegratorsListSort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegratorsListSort {
    /// Most recently registered first (the default).
    #[default]
    Newest,
    /// Alphabetical by name.
    Name,
}

impl IntegratorsListSort {
    fn as_query_str(self) -> &'static str {
        match self {
            IntegratorsListSort::Newest => "newest",
            IntegratorsListSort::Name => "name",
        }
    }
}

/// `GET /integrations`'s query parameters — mirrors
/// `crates/server/src/integrators.rs::ListIntegratorsQuery`.
#[derive(Debug, Clone, Default)]
pub struct ListIntegratorsQuery {
    /// Free-text search over `name`/`slug`/`owner_name`.
    pub q: Option<String>,
    /// Sort order — defaults to [`IntegratorsListSort::Newest`].
    pub sort: IntegratorsListSort,
    /// Page size — server-clamped to its own min/max.
    pub limit: Option<i64>,
    /// The last integrator id from the previous page's results.
    pub cursor: Option<Uuid>,
}

/// One key in an issuer's key history — mirrors
/// `crates/server/src/integrators.rs::IssuerKeyResponse`.
#[derive(Debug, Clone, Deserialize)]
pub struct IssuerKey {
    /// This key's own id.
    pub key_id: Uuid,
    /// `"ed25519"`.
    pub algorithm: String,
    /// `"root"` or `"operational"`.
    pub role: String,
    /// `"attestation"` or `"shard_settlement"`.
    pub purpose: String,
    /// When this key became valid.
    #[serde(with = "time::serde::rfc3339")]
    pub valid_from: OffsetDateTime,
    /// When this key stops being valid, if it has an expiry.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub valid_until: Option<OffsetDateTime>,
    /// When this key was revoked, if it has been.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// The fields [`AvalonClient::add_issuer_key`] needs — mirrors
/// `crates/server/src/integrators.rs::AddIssuerKeyRequest`.
#[derive(Debug, Clone)]
pub struct NewIssuerKey {
    /// `"ed25519"` — the only algorithm registration currently accepts.
    pub algorithm: String,
    /// Standard-base64-encoded raw public key bytes.
    pub public_key: String,
    /// `"root"` or `"operational"`.
    pub role: String,
    /// `"attestation"` (the default) or `"shard_settlement"` (#543).
    pub purpose: Option<String>,
    /// This key's own expiry, if any.
    pub valid_until: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct AddIssuerKeyRequest<'a> {
    algorithm: &'a str,
    public_key: &'a str,
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    purpose: Option<&'a str>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    valid_until: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct RevokeIssuerKeyRequest<'a> {
    reason: Option<&'a str>,
}

/// [`AvalonClient::integrator_whoami`]'s response — mirrors
/// `crates/server/src/integrators.rs::IntegratorWhoamiResponse`.
#[derive(Debug, Clone, Deserialize)]
pub struct IntegratorWhoami {
    /// The integrator id this client's own credential authenticated as.
    pub integrator_id: Uuid,
}

impl AvalonClient {
    /// Requests a fresh challenge and proves this integrator's key is
    /// making the current call — the one proof [`AvalonClient::add_issuer_key`]/
    /// [`AvalonClient::revoke_issuer_key`]/[`AvalonClient::integrator_whoami`]
    /// need. Same shape as `achievements::Session::definition_auth_headers`/
    /// `schema::Session::integrator_auth_headers`, duplicated here since
    /// this module's methods live on [`AvalonClient`] rather than
    /// [`crate::Session`] — see the module doc comment for why.
    async fn integrator_auth_headers(
        &self,
        slug: &str,
    ) -> Result<[(&'static str, String); 3], SdkError> {
        let signing_key_bytes = self
            .config
            .signing_key
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);

        let challenge_response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{slug}/challenge",
                self.config.server_url
            ))
        })
        .await?;
        if !challenge_response.status().is_success() {
            return Err(crate::http::map_error_response(challenge_response).await);
        }
        let challenge: ChallengeResponse = challenge_response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        let nonce = BASE64
            .decode(&challenge.nonce)
            .map_err(|_| SdkError::MissingIssuerCredentials)?;
        let signature = signing_key.sign(&nonce);

        Ok([
            (
                "x-avalon-integrator-key-id",
                self.config.integrator_credential_key_id.clone(),
            ),
            (
                "x-avalon-integrator-challenge-id",
                challenge.challenge_id.to_string(),
            ),
            (
                "x-avalon-integrator-signature",
                BASE64.encode(signature.to_bytes()),
            ),
        ])
    }

    /// `POST /integrations` (closed out by #741/#746) — registers a new
    /// integrator/game/app/service. Unauthenticated: there is no
    /// credential yet to prove. Returns the one-time [`Integrator::credential`]
    /// — persist its `key_id` as `AvalonConfig::integrator_credential_key_id`
    /// for every later call.
    pub async fn register_integrator(
        &self,
        integrator: NewIntegrator,
    ) -> Result<Integrator, SdkError> {
        let body = CreateIntegratorRequest {
            slug: &integrator.slug,
            name: &integrator.name,
            owner_name: &integrator.owner_name,
            requested_capabilities: &integrator.requested_capabilities,
            initial_key: InitialKeyWire {
                algorithm: &integrator.initial_key.algorithm,
                public_key: &integrator.initial_key.public_key,
            },
            category: integrator.category.as_deref(),
        };
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!("{}/integrations", self.config.server_url))
                .json(&body)
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

    /// `GET /integrations?q=&sort=&limit=&cursor=` (issue #270, closed out
    /// by #741/#746) — public, unauthenticated, cursor-paginated.
    pub async fn list_integrators(
        &self,
        query: ListIntegratorsQuery,
    ) -> Result<Vec<IntegratorSummary>, SdkError> {
        let mut params: Vec<(&str, String)> = vec![("sort", query.sort.as_query_str().to_string())];
        if let Some(q) = &query.q {
            params.push(("q", q.clone()));
        }
        if let Some(limit) = query.limit {
            params.push(("limit", limit.to_string()));
        }
        if let Some(cursor) = query.cursor {
            params.push(("cursor", cursor.to_string()));
        }

        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/integrations", self.config.server_url))
                .query(&params)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let body: ListIntegratorsResponseWire = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(body.integrators)
    }

    /// `GET /integrations/{slug}` (closed out by #741/#746) — a public read
    /// of an integrator's registration, no credential fields. Public,
    /// unauthenticated.
    pub async fn get_integrator(&self, slug: &str) -> Result<IntegratorPublic, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/integrations/{slug}", self.config.server_url))
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

    /// `GET /integrations/{slug}/keys` (issue #90, closed out by
    /// #741/#746) — an issuer's full key history (any role, any status),
    /// oldest first. Public, unauthenticated — public keys are already
    /// public by definition.
    pub async fn list_issuer_keys(&self, slug: &str) -> Result<Vec<IssuerKey>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/keys",
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

    /// `POST /integrations/{slug}/keys` (#84, closed out by #741/#746) —
    /// adds a new key to this client's own configured integrator
    /// (`AvalonConfig::integrator_slug`)'s key set. Requires this client's
    /// configured signing key to currently be a **root** key — an
    /// operational key is rejected server-side.
    pub async fn add_issuer_key(&self, key: NewIssuerKey) -> Result<IssuerKey, SdkError> {
        let slug = self
            .config
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;
        let body = AddIssuerKeyRequest {
            algorithm: &key.algorithm,
            public_key: &key.public_key,
            role: &key.role,
            purpose: key.purpose.as_deref(),
            valid_until: key.valid_until,
        };
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/keys",
                    self.config.server_url
                ))
                .json(&body);
            for (name, value) in &headers {
                request = request.header(*name, value);
            }
            request
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

    /// `POST /integrations/{slug}/keys/{key_id}/revoke` (#84, closed out
    /// by #741/#746) — revokes a key (root or operational) in this
    /// client's own configured integrator's key set. Same root-key
    /// requirement as [`AvalonClient::add_issuer_key`]. Revoking an
    /// already-revoked or nonexistent key is rejected, not a silent
    /// no-op.
    pub async fn revoke_issuer_key(
        &self,
        key_id: Uuid,
        reason: Option<&str>,
    ) -> Result<IssuerKey, SdkError> {
        let slug = self
            .config
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;
        let body = RevokeIssuerKeyRequest { reason };
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/keys/{key_id}/revoke",
                    self.config.server_url
                ))
                .json(&body);
            for (name, value) in &headers {
                request = request.header(*name, value);
            }
            request
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

    /// `GET /integrations/whoami` (closed out by #741/#746) — proves this
    /// client's configured integrator credential authenticates, end to
    /// end, over real HTTP. Not a capability-bearing endpoint itself
    /// (issue #27 owns those) — just a way to confirm which integrator id
    /// a credential resolves to.
    pub async fn integrator_whoami(&self) -> Result<IntegratorWhoami, SdkError> {
        let slug = self
            .config
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            let mut request = c.get(format!("{}/integrations/whoami", self.config.server_url));
            for (name, value) in &headers {
                request = request.header(*name, value);
            }
            request
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
    fn integrator_deserializes_from_the_documented_registration_response_shape() {
        let raw = serde_json::json!({
            "id": Uuid::nil(),
            "slug": "ashen-realms",
            "name": "Ashen Realms",
            "owner_name": "Ashen Studios",
            "registered_at": "2026-01-01T00:00:00Z",
            "status": "active",
            "category": "game",
            "requested_capabilities": ["achievements.issue"],
            "credential": { "integrator_id": Uuid::nil(), "key_id": Uuid::nil().to_string() },
        });
        let integrator: Integrator = serde_json::from_value(raw).unwrap();
        assert_eq!(integrator.slug, "ashen-realms");
        assert_eq!(integrator.category, "game");
        assert_eq!(integrator.credential.key_id, Uuid::nil().to_string());
    }

    #[test]
    fn issuer_key_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "key_id": Uuid::nil(),
            "algorithm": "ed25519",
            "role": "root",
            "purpose": "attestation",
            "valid_from": "2026-01-01T00:00:00Z",
            "valid_until": null,
            "revoked_at": null,
        });
        let key: IssuerKey = serde_json::from_value(raw).unwrap();
        assert_eq!(key.role, "root");
        assert!(key.revoked_at.is_none());
    }

    #[test]
    fn integrators_list_sort_maps_to_the_server_s_query_vocabulary() {
        assert_eq!(IntegratorsListSort::Newest.as_query_str(), "newest");
        assert_eq!(IntegratorsListSort::Name.as_query_str(), "name");
    }
}
