//! A thin typed client for the Integrator Registry's external read surface
//! (issue #95) — `GET /registry/{slug}`, matching
//! `crates/server/src/registry.rs::IntegratorRegistryResponse` field-for-
//! field. Public and unauthenticated on the server side, so this lives on
//! [`crate::AvalonClient`] directly rather than [`crate::Session`]: no
//! `authenticate()` call is needed before reading it, the same posture
//! `AvalonClient::authenticate` itself takes before a `Session` exists.
//!
//! **Every field carries its definition and class label, never a bare
//! number** — this type mirrors the server's own `MetricResponse` shape
//! exactly rather than flattening it away, matching this ticket's own
//! invariant that this surface doesn't get to simplify that off for a
//! cleaner response.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{AvalonClient, SdkError, Session};

/// Mirrors `crates/server/src/registry.rs::MetricResponse` at the wire
/// level. `exact: false` means `value` is a coarsened floor (issue #96's
/// minimum-cohort-size privacy safeguard), not the real count — render as
/// "fewer than `value`", never as an exact number, whenever `exact` is
/// `false`.
#[derive(Debug, Clone, Deserialize)]
pub struct Metric {
    /// The metric's value — a coarsened floor, not the real count, when
    /// `exact` is `false`.
    pub value: i64,
    /// A human-readable definition of exactly what's being counted.
    pub definition: String,
    /// A short label naming this metric's own class/category.
    pub class: String,
    /// `false` means `value` is coarsened (#96) — render as "fewer than
    /// `value`", never as an exact count.
    pub exact: bool,
}

/// Mirrors `crates/server/src/registry.rs::IntegratorRegistryResponse` at
/// the wire level.
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    /// Current player count.
    pub players: Metric,
    /// Total distinct players ever, including since-departed ones.
    pub total_players_ever: Metric,
    /// Achievements issued, cumulative.
    pub achievements_issued: Metric,
    /// Achievements revoked, cumulative.
    pub achievements_revoked: Metric,
    /// Distinct identities holding at least one achievement from this
    /// integrator.
    pub unique_achievement_holders: Metric,
}

impl AvalonClient {
    /// `GET /registry/{slug}` — an integrator/issuer's public,
    /// durable-derived metrics. Returns [`SdkError::NotFound`] for a slug
    /// that doesn't exist; an integrator with no activity at all gets
    /// zeros for every field, not an error, same as the server side.
    pub async fn registry(&self, slug: &str) -> Result<Registry, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/registry/{slug}", self.config.server_url))
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

    /// `GET /integrations/{slug}/registry` (closed out by #741/#748) — the
    /// canonical path for exactly the same response
    /// [`AvalonClient::registry`] already reads via issue #95's legacy
    /// `/registry/{slug}` alias (same handler, two routes server-side —
    /// see `crates/server/src/registry.rs`'s own doc comment). Kept as a
    /// separate method rather than folded into `registry` so a caller can
    /// target either route explicitly.
    pub async fn get_integrator_registry(&self, slug: &str) -> Result<Registry, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/registry",
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

    /// `GET /integrations/{slug}/recognitions` (issue #89, closed out by
    /// #741/#748) — every integrator `slug` currently, actively
    /// recognizes. Public, unauthenticated.
    pub async fn list_recognitions(&self, slug: &str) -> Result<Vec<Recognition>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/recognitions",
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

    /// `GET /integrations/{slug}/recognized-by` (issue #89, closed out by
    /// #741/#748) — every integrator that currently, actively recognizes
    /// `slug`. Public, unauthenticated.
    pub async fn list_recognized_by(&self, slug: &str) -> Result<Vec<Recognition>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/recognized-by",
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

/// One published recognition relationship — mirrors
/// `crates/server/src/recognitions.rs::RecognitionResponse` at the wire
/// level. "A used to recognize B, then stopped" isn't representable here:
/// a revoked recognition simply stops appearing in
/// [`AvalonClient::list_recognitions`]/[`AvalonClient::list_recognized_by`]
/// (the server keeps the row, marked `revoked_at`, but never serializes it
/// back into this response shape).
#[derive(Debug, Clone, Deserialize)]
pub struct Recognition {
    /// The integrator declaring recognition.
    pub recognizer_slug: String,
    /// The integrator being recognized.
    pub recognized_slug: String,
    /// The scope this recognition covers — integrator-defined strings,
    /// never interpreted by Avalon itself.
    pub scope: Vec<String>,
    /// When this recognition was last published (or republished).
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
}

#[derive(Deserialize)]
struct ChallengeResponse {
    challenge_id: Uuid,
    nonce: String,
}

#[derive(Serialize)]
struct PublishRecognitionRequest<'a> {
    recognized_slug: &'a str,
    scope: &'a [String],
}

#[derive(Serialize)]
struct RevokeRecognitionRequest<'a> {
    recognized_slug: &'a str,
}

impl Session {
    /// Requests a fresh challenge and proves this integrator's key is
    /// making the current call — the one proof
    /// [`Session::publish_recognition`]/[`Session::revoke_recognition`]
    /// need, with no second, content-specific signature (declaring a
    /// recognition is this integrator's own policy about itself, not a
    /// claim about a player — same posture
    /// `schema::Session::integrator_auth_headers` already documents).
    /// Duplicated per module rather than shared, matching this crate's own
    /// convention.
    async fn recognition_auth_headers(
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

    /// `POST /integrations/{slug}/recognitions` (issue #89, closed out by
    /// #741/#748) — publishes (or republishes) this integrator's own
    /// recognition of `recognized_slug`, for `scope`. Upserted, not
    /// append-only: republishing updates `scope`/`published_at` in place
    /// and clears any prior revocation.
    pub async fn publish_recognition(
        &self,
        slug: &str,
        recognized_slug: &str,
        scope: &[String],
    ) -> Result<Recognition, SdkError> {
        let headers = self.recognition_auth_headers(slug).await?;
        let body = PublishRecognitionRequest {
            recognized_slug,
            scope,
        };
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/recognitions",
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

    /// `POST /integrations/{slug}/recognitions/revoke` (issue #89, closed
    /// out by #741/#748) — marks this integrator's recognition of
    /// `recognized_slug` revoked (the row stays, `revoked_at` set — never
    /// deleted). A no-op, not an error, if no recognition was ever
    /// published.
    pub async fn revoke_recognition(
        &self,
        slug: &str,
        recognized_slug: &str,
    ) -> Result<(), SdkError> {
        let headers = self.recognition_auth_headers(slug).await?;
        let body = RevokeRecognitionRequest { recognized_slug };
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/recognitions/revoke",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "value": 5,
            "definition": "distinct identities with an active GameBinding",
            "class": "durable-derived",
            "exact": true
        });
        let metric: Metric = serde_json::from_value(raw).unwrap();
        assert_eq!(metric.value, 5);
        assert_eq!(metric.class, "durable-derived");
        assert!(metric.exact);
    }

    #[test]
    fn registry_deserializes_all_five_documented_fields() {
        let metric = serde_json::json!({
            "value": 0,
            "definition": "x",
            "class": "durable-derived",
            "exact": true
        });
        let raw = serde_json::json!({
            "players": metric,
            "total_players_ever": metric,
            "achievements_issued": metric,
            "achievements_revoked": metric,
            "unique_achievement_holders": metric,
        });
        let registry: Registry = serde_json::from_value(raw).unwrap();
        assert_eq!(registry.players.value, 0);
        assert_eq!(registry.unique_achievement_holders.class, "durable-derived");
    }

    #[test]
    fn recognition_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "recognizer_slug": "ashen-realms",
            "recognized_slug": "moonlit-vale",
            "scope": ["achievements"],
            "published_at": "2026-01-01T00:00:00Z",
        });
        let recognition: Recognition = serde_json::from_value(raw).unwrap();
        assert_eq!(recognition.recognizer_slug, "ashen-realms");
        assert_eq!(recognition.recognized_slug, "moonlit-vale");
        assert_eq!(recognition.scope, vec!["achievements".to_string()]);
    }
}
