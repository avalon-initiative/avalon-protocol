//! Integrator Space schema publication (issue #386), on top of the real
//! wire contract issues #255/#381/#384 settled server-side
//! (`crates/server/src/integrator_schemas.rs`/`integrator_data.rs`). No
//! wrapper for this existed anywhere in the SDK before this ticket — an
//! integrator using the Rust SDK had to hand-write raw `.proto` source and
//! build the request by hand.
//!
//! [`AvalonSchema`] is the trait `#[derive(AvalonSchema)]`
//! (`avalon_schema_derive`, re-exported below) implements: given an
//! ordinary Rust struct, it generates the equivalent `.proto` `message`
//! text and the `default_visibility`/`field_visibility` maps #381/#384
//! need, so an integrator writes `#[derive(AvalonSchema, Serialize)]`
//! once and never sees protobuf syntax. The struct doubles as the
//! type-safe instance shape: publishing an instance is
//! `session.publish_instance(version, &my_struct).await?` — ordinary
//! `serde_json::to_value` on the struct, not a second, separately
//! maintained serialization path, since the generated proto field names
//! match the struct's own field names exactly (see
//! `avalon-schema-derive`'s own doc comment on why field order/naming
//! matters here) and `protobuf-json-mapping`'s JSON parser accepts a
//! message's original (snake_case) field names, not only the camelCase
//! form its own printer would emit — confirmed by this module's own live
//! test, not assumed.
//!
//! **Auth**: the exact same integrator challenge-response ceremony
//! `achievements.rs::submit_achievement_issuance` already uses (proof
//! that this integrator's key is making this call, right now) — but,
//! unlike achievement issuance, neither endpoint here needs a *second*,
//! content-specific signature. `publish_schema_version`/`publish_instance`
//! only ever check "is the caller genuinely this integrator," never "did
//! this integrator specifically authorize *this* schema/instance" the way
//! an attestation's embedded signature proves — see those handlers' own
//! doc comments server-side.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AvalonClient, SdkError, Session};

pub use avalon_schema_derive::AvalonSchema;

/// Implemented by `#[derive(AvalonSchema)]` — see the module doc comment
/// and `avalon-schema-derive`'s own doc comment for the full contract
/// (supported field types, attribute vocabulary, field numbering).
pub trait AvalonSchema {
    /// The generated `.proto` message text — always exactly one top-level
    /// `message`, matching `crates/server/src/proto_schema.rs`'s own
    /// requirement.
    fn proto_source() -> String;
    /// `"public"` (the default) or `"private"` — #381's schema-level
    /// opt-out.
    fn default_visibility() -> &'static str;
    /// Field name -> `"public"`/`"private"`, only for fields carrying an
    /// explicit `#[avalon(visibility = "...")]` attribute.
    fn field_visibility() -> BTreeMap<String, String>;
}

#[derive(Deserialize)]
struct ChallengeResponse {
    challenge_id: Uuid,
    nonce: String,
}

/// Mirrors `crates/server/src/integrator_schemas.rs::IntegratorSchemaVersionResponse`
/// at the wire level.
#[derive(Debug, Clone, Deserialize)]
pub struct SchemaVersion {
    /// This version's own namespaced id, e.g. `"game:<slug>:schema:<version>"`.
    pub id: String,
    /// The publishing integrator's id.
    pub integrator_id: Uuid,
    /// Monotonic version number, starting at 1.
    pub version: u32,
    /// The generated `.proto` message text.
    pub proto_source: String,
    /// When this version was published, RFC3339.
    pub published_at: String,
    /// This version's own id, if a newer version has since superseded it.
    pub superseded_by: Option<String>,
    /// `"public"` or `"private"` — the schema-level visibility default.
    pub default_visibility: String,
    /// Field name -> `"public"`/`"private"` overrides of the default.
    pub field_visibility: BTreeMap<String, String>,
}

/// Mirrors `crates/server/src/integrator_data.rs::IntegratorDataInstanceResponse`
/// at the wire level.
#[derive(Debug, Clone, Deserialize)]
pub struct DataInstance {
    /// This instance's own id.
    pub id: String,
    /// The schema version this instance conforms to.
    pub schema_id: String,
    /// The publishing integrator's id.
    pub integrator_id: Uuid,
    /// The identity this instance is about.
    pub subject: Uuid,
    /// The instance data itself, as validated against the schema.
    pub instance: serde_json::Value,
    /// When this instance was published, RFC3339.
    pub published_at: String,
    /// This instance's own id, if a newer instance for the same
    /// `(schema, subject)` has since superseded it.
    pub superseded_by: Option<String>,
}

/// Mirrors
/// `crates/server/src/integrator_schema_mappings.rs::IntegratorSchemaMappingResponse`
/// at the wire level — issue #491.
#[derive(Debug, Clone, Deserialize)]
pub struct SchemaMapping {
    /// This mapping's own namespaced id, e.g. `"game:<slug>:schema_mapping:<seq>"`.
    pub id: String,
    /// The publishing integrator's id.
    pub integrator_id: Uuid,
    /// The schema version this mapping maps *from*.
    pub from_schema_id: String,
    /// The schema version this mapping maps *to*.
    pub to_schema_id: String,
    /// Free text documenting whatever `field_correspondence` can't
    /// capture (merges, splits, dropped fields, default values).
    pub description: String,
    /// A simple old-field -> new-field correspondence map. Never
    /// interpreted or executed by Avalon — the integrator owns the
    /// semantic transformation this describes.
    pub field_correspondence: BTreeMap<String, String>,
    /// When this mapping was published, RFC3339.
    pub published_at: String,
}

/// One currently-visible Integrator Space instance about an identity —
/// mirrors `crates/server/src/integrator_data.rs::VisibleIntegratorDataInstanceResponse`
/// at the wire level. Unlike [`DataInstance`] this carries only the fields
/// its schema's visibility rules currently allow, never the full instance.
#[derive(Debug, Clone, Deserialize)]
pub struct VisibleDataInstance {
    /// The schema version this instance conforms to.
    pub schema: String,
    /// The publishing integrator's id.
    pub integrator_id: Uuid,
    /// When this instance was published, RFC3339.
    pub published_at: String,
    /// Only the fields the instance's schema currently makes visible.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
struct DeleteInstanceRequest<'a> {
    reason_code: &'a str,
    reason: Option<&'a str>,
}

#[derive(Serialize)]
struct PublishSchemaVersionRequest {
    proto_source: String,
    default_visibility: String,
    field_visibility: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct PublishMappingRequest<'a> {
    from_schema_id: &'a str,
    to_schema_id: &'a str,
    description: &'a str,
    field_correspondence: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct PublishInstanceRequest<'a, T> {
    subject: Uuid,
    instance: &'a T,
}

impl Session {
    /// Requests a fresh challenge and proves this integrator's key is
    /// making the current call — the one proof both endpoints below need,
    /// factored out since neither needs the second, content-specific
    /// signature `achievements.rs::submit_achievement_issuance` also does
    /// (see this module's own doc comment).
    async fn integrator_auth_headers(
        &self,
        slug: &str,
    ) -> Result<[(&'static str, String); 3], SdkError> {
        let signing_key_bytes = self.signing_key.ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);

        let challenge_response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!("{}/integrations/{slug}/challenge", self.server_url))
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
            ("x-avalon-integrator-key-id", self.integrator_key_id.clone()),
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

    /// `POST /integrations/{slug}/schemas` — publishes the next version of
    /// this integrator's Game Space schema, generated from `T` via
    /// `#[derive(AvalonSchema)]`. Always a new version; there is no
    /// "update an existing schema" call, matching the server's own
    /// immutability invariant.
    pub async fn publish_schema_version<T: AvalonSchema>(&self) -> Result<SchemaVersion, SdkError> {
        let slug = self
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;

        let body = PublishSchemaVersionRequest {
            proto_source: T::proto_source(),
            default_visibility: T::default_visibility().to_string(),
            field_visibility: T::field_visibility(),
        };

        // No idempotency key on this write yet (documented follow-up, see
        // this module's doc comment) — one attempt, no automatic retry.
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!("{}/integrations/{slug}/schemas", self.server_url))
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

    /// `POST /integrations/{slug}/mappings` (issue #491) — publishes a
    /// mapping documenting a correspondence between two of this
    /// integrator's own already-published schema versions. Not an
    /// execution engine: `field_correspondence` is a simple old-field ->
    /// new-field rename map, `description` is free text for whatever that
    /// map can't capture (merges, splits, dropped fields, default
    /// values) — Avalon never interprets or runs either. Both
    /// `from_schema_id`/`to_schema_id` must already be published and owned
    /// by this integrator; the server rejects (never silently accepts) a
    /// reference to a schema it doesn't own or that doesn't exist.
    pub async fn publish_mapping(
        &self,
        from_schema_id: &str,
        to_schema_id: &str,
        description: &str,
        field_correspondence: BTreeMap<String, String>,
    ) -> Result<SchemaMapping, SdkError> {
        let slug = self
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;

        let body = PublishMappingRequest {
            from_schema_id,
            to_schema_id,
            description,
            field_correspondence: &field_correspondence,
        };

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!("{}/integrations/{slug}/mappings", self.server_url))
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

    /// `POST /integrations/{slug}/schemas/{version}/data` — publishes (or
    /// supersedes) this integrator's instance data, against the schema
    /// version named by `version`, for this session's own identity. The
    /// subject identity must have an active binding to this integrator —
    /// the same "the user's own consent" gate
    /// `Session::issue_achievement` requires, enforced server-side.
    pub async fn publish_instance<T: AvalonSchema + Serialize>(
        &self,
        version: u32,
        instance: &T,
    ) -> Result<DataInstance, SdkError> {
        let slug = self
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;

        let body = PublishInstanceRequest {
            subject: self.identity.id.0,
            instance,
        };

        // Same posture as `publish_schema_version` above: no idempotency
        // key yet, one attempt only.
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/schemas/{version}/data",
                    self.server_url
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

    /// `DELETE /integrations/{slug}/schemas/{version}/data/{subject}`
    /// (#533, closed out by #741/#745) — an append-only tombstone for this
    /// integrator's current instance belonging to `subject` under this
    /// schema version: the original instance row is never touched, only
    /// marked deleted (`docs/architecture/revocation.md`'s pattern) — the
    /// original publish event, and the delete event this appends, both
    /// stay observable in raw ledger history. Same auth posture as
    /// [`Session::publish_schema_version`]: this integrator's own
    /// challenge-response proof only, no second content-specific
    /// signature.
    pub async fn delete_instance(
        &self,
        version: u32,
        subject: Uuid,
        reason_code: &str,
        reason: Option<&str>,
    ) -> Result<(), SdkError> {
        let slug = self
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let headers = self.integrator_auth_headers(slug).await?;

        let body = DeleteInstanceRequest {
            reason_code,
            reason,
        };

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .delete(format!(
                    "{}/integrations/{slug}/schemas/{version}/data/{subject}",
                    self.server_url
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
        Ok(())
    }
}

impl AvalonClient {
    /// `GET /integrations/{slug}/mappings` (issue #491) — every mapping
    /// `slug` has published, oldest first. Public and unauthenticated, like
    /// schema-version listing — lives on [`AvalonClient`] rather than
    /// [`Session`] for the same reason [`AvalonClient::registry`] does: no
    /// `authenticate()` call is needed before reading it.
    pub async fn list_schema_mappings(&self, slug: &str) -> Result<Vec<SchemaMapping>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/mappings",
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

    /// `GET /integrations/{slug}/mappings/{seq}` — one published mapping,
    /// verbatim. Public and unauthenticated.
    pub async fn get_schema_mapping(
        &self,
        slug: &str,
        seq: u32,
    ) -> Result<SchemaMapping, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/mappings/{seq}",
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

    /// `GET /integrations/{slug}/schemas` (closed out by #741/#745) — every
    /// published version for this integrator, oldest first. Public and
    /// unauthenticated, same visibility as [`AvalonClient::list_schema_mappings`].
    /// Empty for an integrator that has never published.
    pub async fn list_schema_versions(&self, slug: &str) -> Result<Vec<SchemaVersion>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/schemas",
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

    /// `GET /integrations/{slug}/schemas/{version}` (closed out by
    /// #741/#745) — one published version, verbatim. Public,
    /// unauthenticated — the same round-trip check
    /// [`Session::publish_schema_version`]'s own module doc comment
    /// describes.
    pub async fn get_schema_version(
        &self,
        slug: &str,
        version: u32,
    ) -> Result<SchemaVersion, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/schemas/{version}",
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

    /// `GET /identities/{id}/integrator-data` (#384, closed out by
    /// #741/#745) — every current (non-superseded, non-deleted) Integrator
    /// Space instance published about `identity_id`, across every
    /// integrator/schema, each already filtered server-side to only the
    /// fields its schema currently makes visible (#381's visibility rules
    /// — this SDK never re-applies or second-guesses that filtering).
    /// Public, unauthenticated.
    pub async fn identity_integrator_data(
        &self,
        identity_id: Uuid,
    ) -> Result<Vec<VisibleDataInstance>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/identities/{identity_id}/integrator-data",
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
mod visible_data_instance_tests {
    use super::*;

    #[test]
    fn visible_data_instance_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "schema": "game:ashen-realms:schema:1",
            "integrator_id": Uuid::nil(),
            "published_at": "2026-01-01T00:00:00Z",
            "fields": { "level": 42 },
        });
        let instance: VisibleDataInstance = serde_json::from_value(raw).unwrap();
        assert_eq!(instance.schema, "game:ashen-realms:schema:1");
        assert_eq!(instance.fields.get("level").unwrap(), 42);
    }
}
