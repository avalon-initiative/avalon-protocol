//! Achievements (issue #34): read a user's own attestation history and
//! issue new attestations on this integrator's own behalf. Previously both
//! `Session` methods here returned `SdkError::NotImplemented`.
//!
//! **Reading** (`Session::achievements`) uses the identity's own bearer
//! token against `GET /me/achievements` — an identity reading its own full
//! history, the same "facts, not a per-consumer verdict" posture `GET
//! /attestations/{id}` (#33) already takes: `authenticity`/`validity` come
//! back as the server computed them. **Recognition is never present
//! here** — that's `avalon_protocol::achievements::recognize`, evaluated
//! by this integrator against its own `TrustRelationship`, matching ADR
//! #76's "recognition is contextual" rule. This SDK doesn't compute a
//! recognition verdict on the caller's behalf either; an integrator that wants
//! one filters [`VerifiedAttestation`]s through its own policy.
//!
//! **Issuing** (`Session::issue_achievement`) needs this integrator's own
//! signing key — the server never sees it, only a detached signature
//! (`AvalonConfig::integrator_slug`/`signing_key`). Two independent proofs go
//! out, mirroring every other issuer-credentialed endpoint in this repo
//! (`docs/architecture/issuers.md`): an ephemeral
//! challenge-response proving *this key* is making the HTTP call right now
//! (`POST /integrations/{slug}/challenge`), and a separate signature embedded in
//! the request body over the attestation's own canonical bytes, proving
//! *this key* specifically authorized *this* attestation — checked
//! independently server-side
//! (`avalon_chain::attestations::verify_authenticity`), not inferred from
//! the HTTP-level proof alone.
//!
//! Milestones (the App/Service equivalent, #324/#325) aren't wired up
//! here — this ticket's own scope is `achievements()`/`issue_achievement()`
//! specifically; a `milestones()`/`issue_milestone()` pair would follow the
//! same shape against `/integrations/{slug}/milestones/{key}/issue`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{SdkError, Session};

/// Mirrors `avalon_chain::attestations::Authenticity` at the wire level.
/// This crate defines its own copy rather than depending on `avalon-chain`
/// directly — an integrator has no business linking the verification
/// engine itself, only its result — the same posture every other SDK
/// response type in this crate already takes (e.g. `social::Friend`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Authenticity {
    /// The embedded signature verifies against one of the issuer's keys.
    Authentic {
        /// Which of the issuer's keys verified it.
        key_id: String,
    },
    /// The embedded signature does not verify against any of the issuer's
    /// currently-known keys.
    NotAuthentic {
        /// Why — never used to make an authorization decision, only to
        /// explain a rejection to a developer.
        reason: String,
    },
}

/// Mirrors `avalon_protocol::achievements::Validity` at the wire level.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Validity {
    /// Not revoked, and its definition isn't retired in a way that
    /// invalidates already-issued attestations.
    Valid,
    /// Revoked, or otherwise no longer honored.
    Invalid {
        /// Why — see `crate::attestations::Validity` server-side for the
        /// exact set of reasons.
        reason: String,
    },
}

/// One entry in an attestation's history (`"issued"`, plus `"revoked"` if
/// applicable, #85) — see `crate::attestations::AttestationHistoryEntry`
/// server-side.
#[derive(Debug, Clone, Deserialize)]
pub struct AttestationHistoryEntry {
    /// `"issued"` or `"revoked"`.
    pub event: String,
    /// When this history event happened.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// A machine-readable reason code, present on a `"revoked"` entry.
    pub reason_code: Option<String>,
    /// A human-readable reason, present on a `"revoked"` entry.
    pub reason: Option<String>,
}

/// One attestation from the caller's own history, with its computed
/// authenticity and validity attached — deliberately no `recognition`
/// field, see module doc comment.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifiedAttestation {
    /// This attestation's own id.
    pub id: Uuid,
    /// The issuer that issued it, e.g. `"game:<slug>"`.
    pub issuer: String,
    /// The identity this attestation is about.
    pub subject: Uuid,
    /// The achievement/milestone definition this attestation claims,
    /// e.g. `"game:<slug>:achievement:<key>"`.
    pub achievement: String,
    /// When it was issued.
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    /// Whether the embedded signature actually verifies.
    pub authenticity: Authenticity,
    /// Whether it's still in force (not revoked).
    pub validity: Validity,
    /// Its full history — issuance, and revocation if any.
    pub history: Vec<AttestationHistoryEntry>,
}

#[derive(Deserialize)]
struct ChallengeResponse {
    challenge_id: Uuid,
    nonce: String,
}

#[derive(Serialize)]
struct IssueRequest {
    key_id: Uuid,
    signature: String,
}

#[derive(Deserialize)]
struct IssueResponse {
    id: Uuid,
}

/// The exact bytes this integrator's key signs to authorize an
/// attestation — must match
/// `avalon_protocol::achievements::attestation_signing_bytes` exactly.
/// This crate defines its own copy rather than depending on the server's
/// private construction: each side of the wire independently builds the
/// same canonical format, the same posture every other signed request in
/// this repo already takes (client and server never share a signing-bytes
/// function, only its documented shape).
fn attestation_signing_bytes(issuer_ref: &str, subject: Uuid, achievement: &str) -> Vec<u8> {
    format!("avalon:achievement.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

/// The exact bytes this integrator's key signs to authorize a *bulk*
/// issuance (issue #495, implementing #492's decided shape) — must match
/// `avalon_protocol::achievements::bulk_attestation_signing_bytes` exactly.
/// Same "each side independently builds the same canonical format" posture
/// as [`attestation_signing_bytes`] above.
fn bulk_attestation_signing_bytes(
    issuer_ref: &str,
    subject: Uuid,
    achievements: &[String],
) -> Vec<u8> {
    let mut message =
        format!("avalon:achievement.issued.bulk:v1:{issuer_ref}:{subject}:").into_bytes();
    message.extend_from_slice(&(achievements.len() as u32).to_be_bytes());
    for achievement in achievements {
        message.extend_from_slice(&(achievement.len() as u32).to_be_bytes());
        message.extend_from_slice(achievement.as_bytes());
    }
    message
}

#[derive(Serialize)]
struct BulkClaimRequestWire {
    key: String,
}

#[derive(Serialize)]
struct BulkIssueRequest {
    key_id: Uuid,
    signature: String,
    claims: Vec<BulkClaimRequestWire>,
}

/// The attestation a successful bulk claim resulted in — a deliberately
/// smaller shape than the full server response (omitting `proof`, which
/// nothing in this SDK's own bulk caller needs), same minimalism
/// `Session::issue_achievement`'s bare `Uuid` return already takes for the
/// single-claim case.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkIssuedAttestation {
    /// This attestation's own id.
    pub id: Uuid,
    /// The issuer that issued it, e.g. `"game:<slug>"`.
    pub issuer: String,
    /// The identity this attestation is about.
    pub subject: Uuid,
    /// The achievement definition this attestation claims, e.g.
    /// `"game:<slug>:achievement:<key>"`.
    pub achievement: String,
    /// When it was issued.
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
}

/// One claim's own outcome from a bulk issuance call — a bulk call is
/// never all-or-nothing (#495's own invariant): a claim referencing an
/// unknown or retired definition fails on its own, every other claim in
/// the same call still succeeds. Mirrors
/// `crates/server/src/achievements.rs::BulkClaimResult` at the wire level.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BulkClaimOutcome {
    /// This claim was issued successfully.
    Issued {
        /// The achievement key this outcome is for.
        key: String,
        /// The resulting attestation.
        attestation: BulkIssuedAttestation,
    },
    /// This claim failed — every other claim in the same call may still
    /// have succeeded; check each [`BulkClaimOutcome`] independently.
    Failed {
        /// The achievement key this outcome is for.
        key: String,
        /// A stable, machine-readable reason code.
        code: String,
        /// A human-readable explanation of `code`.
        error: String,
    },
}

#[derive(Deserialize)]
struct BulkIssueResponseWire {
    results: Vec<BulkClaimOutcome>,
}

/// Mirrors `crate::attestations::ListMyAchievementsResponse` at the wire
/// level — issue #377 wrapped what was a bare array in a
/// `{ achievements, next_cursor }` envelope so `GET /me/achievements`
/// could be paginated. `fetch_achievements` below unwraps this and returns
/// just the first page's attestations, matching `Session::achievements`'s
/// existing, unpaginated public shape; a paginated/filtered entry point is
/// a follow-up, not built here (see issue #377's own PR for why the SDK
/// side was scoped out).
#[derive(Deserialize)]
struct ListMyAchievementsResponse {
    achievements: Vec<VerifiedAttestation>,
    #[allow(dead_code)]
    next_cursor: Option<Uuid>,
}

impl Session {
    pub(crate) async fn fetch_achievements(&self) -> Result<Vec<VerifiedAttestation>, SdkError> {
        // `limit=200` (the server's own max page size,
        // `attestations::MAX_ACHIEVEMENTS_PAGE_SIZE`) rather than the
        // default 50 — minimizes the behavior change from before #377's
        // pagination landed, though a caller with more than 200
        // attestations from a single identity now genuinely needs the
        // (not yet built) paginated entry point to see the rest.
        let response = crate::http::send(&self.http, &self.retry, true, |c| {
            c.get(format!("{}/me/achievements?limit=200", self.server_url))
                .bearer_auth(&self.token)
        })
        .await?;

        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }

        let page: ListMyAchievementsResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(page.achievements)
    }

    /// Issue #47's own named example: this write carries a fresh
    /// `Idempotency-Key`, generated once per logical call and reused
    /// across every retry attempt of it (never regenerated per attempt —
    /// that would defeat the point), so a retried issuance replays the
    /// first attempt's result instead of minting a second attestation —
    /// see `crate::idempotency` server-side. The retry unit is the *whole*
    /// challenge-then-issue exchange, not just the final POST: a
    /// challenge is single-use, so retrying only the issue request with a
    /// possibly-already-consumed challenge id would fail differently
    /// rather than actually retry (`crate::http::retry_write`'s own doc
    /// comment).
    pub(crate) async fn submit_achievement_issuance(&self, key: &str) -> Result<Uuid, SdkError> {
        let idempotency_key = Uuid::new_v4().to_string();
        crate::http::retry_write(&self.retry, || {
            self.attempt_achievement_issuance(key, &idempotency_key)
        })
        .await
    }

    async fn attempt_achievement_issuance(
        &self,
        key: &str,
        idempotency_key: &str,
    ) -> Result<Uuid, SdkError> {
        let slug = self
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key_bytes = self.signing_key.ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);
        let key_id: Uuid = self
            .integrator_key_id
            .parse()
            .map_err(|_| SdkError::MissingIssuerCredentials)?;

        // Proof one: this key is making this HTTP call, right now. Not
        // retried by `http::send` itself — a failed attempt here means
        // `retry_write` redoes this whole function, fetching a fresh
        // challenge along with it.
        let challenge_response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{}/challenge",
                self.server_url, slug
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
        let challenge_signature = signing_key.sign(&nonce);

        // Proof two: this key specifically authorized this attestation —
        // independent of the challenge-response above, checked
        // server-side against the same canonical bytes.
        let subject = self.identity.id.0;
        let issuer_ref = format!("game:{slug}");
        let achievement = format!("game:{slug}:achievement:{key}");
        let signing_bytes = attestation_signing_bytes(&issuer_ref, subject, &achievement);
        let signature = signing_key.sign(&signing_bytes);

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{}/achievements/{}/issue",
                self.server_url, slug, key
            ))
            .header("x-avalon-integrator-key-id", &self.integrator_key_id)
            .header(
                "x-avalon-integrator-challenge-id",
                challenge.challenge_id.to_string(),
            )
            .header(
                "x-avalon-integrator-signature",
                BASE64.encode(challenge_signature.to_bytes()),
            )
            .header("x-avalon-identity-id", subject.to_string())
            .header("idempotency-key", idempotency_key)
            .json(&IssueRequest {
                key_id,
                signature: BASE64.encode(signature.to_bytes()),
            })
        })
        .await?;

        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }

        let body: IssueResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(body.id)
    }

    /// Issue #495 (implementing #492's decided shape): the bulk-issuance
    /// counterpart to [`Session::submit_achievement_issuance`] — one
    /// challenge-response, one signature over the whole ordered `keys`
    /// list, every claim still becoming its own ordinary attestation
    /// server-side. Same idempotency/retry posture as the single-claim
    /// call: the *whole* challenge-then-bulk-issue exchange is the retry
    /// unit, never just the final POST.
    pub(crate) async fn submit_bulk_achievement_issuance(
        &self,
        keys: &[&str],
    ) -> Result<Vec<BulkClaimOutcome>, SdkError> {
        let idempotency_key = Uuid::new_v4().to_string();
        crate::http::retry_write(&self.retry, || {
            self.attempt_bulk_achievement_issuance(keys, &idempotency_key)
        })
        .await
    }

    async fn attempt_bulk_achievement_issuance(
        &self,
        keys: &[&str],
        idempotency_key: &str,
    ) -> Result<Vec<BulkClaimOutcome>, SdkError> {
        let slug = self
            .integrator_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key_bytes = self.signing_key.ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);
        let key_id: Uuid = self
            .integrator_key_id
            .parse()
            .map_err(|_| SdkError::MissingIssuerCredentials)?;

        let challenge_response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{}/challenge",
                self.server_url, slug
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
        let challenge_signature = signing_key.sign(&nonce);

        let subject = self.identity.id.0;
        let issuer_ref = format!("game:{slug}");
        let achievements: Vec<String> = keys
            .iter()
            .map(|key| format!("game:{slug}:achievement:{key}"))
            .collect();
        let signing_bytes = bulk_attestation_signing_bytes(&issuer_ref, subject, &achievements);
        let signature = signing_key.sign(&signing_bytes);

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{}/achievements/bulk-issue",
                self.server_url, slug
            ))
            .header("x-avalon-integrator-key-id", &self.integrator_key_id)
            .header(
                "x-avalon-integrator-challenge-id",
                challenge.challenge_id.to_string(),
            )
            .header(
                "x-avalon-integrator-signature",
                BASE64.encode(challenge_signature.to_bytes()),
            )
            .header("x-avalon-identity-id", subject.to_string())
            .header("idempotency-key", idempotency_key)
            .json(&BulkIssueRequest {
                key_id,
                signature: BASE64.encode(signature.to_bytes()),
                claims: keys
                    .iter()
                    .map(|key| BulkClaimRequestWire {
                        key: key.to_string(),
                    })
                    .collect(),
            })
        })
        .await?;

        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }

        let body: BulkIssueResponseWire = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(body.results)
    }
}
