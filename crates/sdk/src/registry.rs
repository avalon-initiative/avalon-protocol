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

use serde::Deserialize;

use crate::{AvalonClient, SdkError};

/// Mirrors `crates/server/src/registry.rs::MetricResponse` at the wire
/// level. `exact: false` means `value` is a coarsened floor (issue #96's
/// minimum-cohort-size privacy safeguard), not the real count — render as
/// "fewer than `value`", never as an exact number, whenever `exact` is
/// `false`.
#[derive(Debug, Clone, Deserialize)]
pub struct Metric {
    pub value: i64,
    pub definition: String,
    pub class: String,
    pub exact: bool,
}

/// Mirrors `crates/server/src/registry.rs::IntegratorRegistryResponse` at
/// the wire level.
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    pub players: Metric,
    pub total_players_ever: Metric,
    pub achievements_issued: Metric,
    pub achievements_revoked: Metric,
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
}
