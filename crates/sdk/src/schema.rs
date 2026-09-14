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

use crate::{SdkError, Session};

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

#[derive(Serialize)]
struct PublishSchemaVersionRequest {
    proto_source: String,
    default_visibility: String,
    field_visibility: BTreeMap<String, String>,
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
}
