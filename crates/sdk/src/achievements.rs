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
//! Milestones (the App/Service equivalent, #324/#325, closed out by #741/
//! #744): [`Session::issue_milestone`]/[`Session::bulk_issue_milestones`]
//! follow [`Session::submit_achievement_issuance`]/
//! [`Session::submit_bulk_achievement_issuance`]'s exact shape against
//! `/integrations/{slug}/milestones/{key}/issue` /
//! `/integrations/{slug}/milestones/bulk-issue` — the one difference is the
//! issuer prefix, since a milestone issuer is always an App or Service
//! (never a Game, #324's category split), so callers pass their own
//! registered [`crate::types::integrators::IntegratorCategory`] rather
//! than this module hardcoding `"game:"`.
//!
//! #744 also closes the achievement/milestone *definition* CRUD gap: create/
//! update ([`Session::create_achievement_definition`]/
//! [`Session::update_achievement_definition`]/[`Session::create_milestone_definition`]/
//! [`Session::update_milestone_definition`]) need only this integrator's own
//! challenge-response proof (no second, content-specific signature — a
//! definition isn't a claim about a player, matching
//! `schema::Session::publish_schema_version`'s auth posture, not
//! [`Session::issue_achievement`]'s), so they live here on [`Session`]
//! reusing that same two-header-fetch dance. Listing definitions
//! (`AvalonClient::list_achievement_definitions`/
//! `AvalonClient::list_milestone_definitions`) and reading a single
//! attestation by id (`AvalonClient::get_attestation`, mirroring `GET
//! /attestations/{id}`'s own "facts, not a verdict" posture — reuses
//! [`VerifiedAttestation`] rather than a second, parallel response type)
//! are public and unauthenticated, so they live on
//! [`crate::AvalonClient`] instead, same split `registry.rs`/`schema.rs`
//! already establish between public reads and integrator-credentialed
//! writes.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::types::integrators::IntegratorCategory;

use crate::{AvalonClient, SdkError, Session};

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
/// attestation. `claim_kind` is `"achievement"` or `"milestone"`
/// ([`IntegratorCategory::claim_kind`]) — folded into the signed bytes so a
/// signature produced for one claim vocabulary can never be replayed as if
/// it were the other, even though the wire mechanics are identical.
///
/// This crate builds these bytes itself rather than calling the server's
/// own construction (`avalon_protocol::achievements::attestation_signing_bytes`)
/// — the same posture the C# and TypeScript SDKs already take. The two
/// implementations agreeing is a documented contract checked by
/// `conformance/vectors/attestation-signing.json`, not something the
/// compiler enforces; public here so an integrator can verify what it is
/// about to sign.
pub fn attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: Uuid,
    achievement: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

/// The exact bytes this integrator's key signs to authorize a *bulk*
/// issuance (issue #495): one signature over the whole ordered
/// `achievements` list for one `subject`, each entry length-prefixed
/// big-endian so two different orderings of the same keys — or a key
/// containing bytes that could otherwise be mistaken for a delimiter —
/// can never produce identical signed bytes.
///
/// Same independently-implemented-and-vector-checked posture as
/// [`attestation_signing_bytes`] above.
pub fn bulk_attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: Uuid,
    achievements: &[String],
) -> Vec<u8> {
    let mut message =
        format!("avalon:{claim_kind}.issued.bulk:v1:{issuer_ref}:{subject}:").into_bytes();
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

/// The exact bytes this integrator's key signs to authorize a revocation
/// (issue #85, wrapped here by #498). `attestation_id` folded in means a
/// revocation signature can never be replayed against a different
/// attestation; `reason_code` folded in means it can't be replayed with a
/// different claimed reason either.
///
/// Same independently-implemented-and-vector-checked posture as
/// [`attestation_signing_bytes`] above.
pub fn revocation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    attestation_id: Uuid,
    reason_code: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.revoked:v1:{issuer_ref}:{attestation_id}:{reason_code}")
        .into_bytes()
}

#[derive(Serialize)]
struct RevokeAttestationRequest {
    key_id: Uuid,
    signature: String,
    reason_code: String,
    reason: String,
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
        let claim_kind = IntegratorCategory::Game.claim_kind();
        let issuer_ref = format!("game:{slug}");
        let achievement = format!("game:{slug}:achievement:{key}");
        let signing_bytes =
            attestation_signing_bytes(claim_kind, &issuer_ref, subject, &achievement);
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
        let claim_kind = IntegratorCategory::Game.claim_kind();
        let issuer_ref = format!("game:{slug}");
        let achievements: Vec<String> = keys
            .iter()
            .map(|key| format!("game:{slug}:achievement:{key}"))
            .collect();
        let signing_bytes =
            bulk_attestation_signing_bytes(claim_kind, &issuer_ref, subject, &achievements);
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

    /// `POST /attestations/{id}/revoke` (issue #85, wrapped here by #498)
    /// — revokes an attestation this integrator itself issued (singly or
    /// via `issue_achievements_bulk`, which doesn't distinguish a bulk-
    /// issued attestation from any other — see module doc comment: no
    /// bulk-revoke mechanism exists or is needed, since every claim in a
    /// bulk call is already its own ordinary, independently-revocable
    /// attestation). Same two-proof shape as issuance: a fresh challenge-
    /// response proving this key is making the call right now, plus a
    /// signature embedded in the body over the revocation's own canonical
    /// bytes, checked independently server-side. No idempotency key —
    /// unlike issuance, a bare retry here is already safe: the server's
    /// own already-revoked check (`SdkError::Conflict`) makes a repeat
    /// call a no-op, never a second revocation.
    pub async fn revoke_attestation(
        &self,
        attestation_id: Uuid,
        reason_code: &str,
        reason: &str,
    ) -> Result<(), SdkError> {
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

        let claim_kind = IntegratorCategory::Game.claim_kind();
        let issuer_ref = format!("game:{slug}");
        let signing_bytes =
            revocation_signing_bytes(claim_kind, &issuer_ref, attestation_id, reason_code);
        let signature = signing_key.sign(&signing_bytes);

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/attestations/{attestation_id}/revoke",
                self.server_url
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
            .json(&RevokeAttestationRequest {
                key_id,
                signature: BASE64.encode(signature.to_bytes()),
                reason_code: reason_code.to_string(),
                reason: reason.to_string(),
            })
        })
        .await?;

        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        Ok(())
    }

    /// Requests a fresh challenge and proves this integrator's key is
    /// making the current call — the one proof [`Session::create_achievement_definition`]/
    /// [`Session::update_achievement_definition`]/[`Session::create_milestone_definition`]/
    /// [`Session::update_milestone_definition`] need, with no second,
    /// content-specific signature (a definition isn't a claim about a
    /// player). Same shape as `schema::Session::integrator_auth_headers`,
    /// duplicated rather than shared across modules — see this crate's own
    /// convention of small, module-local auth helpers rather than a shared
    /// one.
    async fn definition_auth_headers(
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

    /// `POST /integrations/{slug}/achievements` (#324/#325, closed out by
    /// #744) — defines a new achievement this Game integrator can later
    /// issue. Idempotent in the sense that a duplicate `key` for this
    /// issuer is rejected ([`SdkError::Conflict`]), never silently
    /// creating a second definition.
    pub async fn create_achievement_definition(
        &self,
        slug: &str,
        definition: NewClaimDefinition,
    ) -> Result<AchievementDefinition, SdkError> {
        let headers = self.definition_auth_headers(slug).await?;
        let body = CreateClaimDefinitionRequest {
            key: &definition.key,
            name: &definition.name,
            description: &definition.description,
            schema: definition.schema.as_deref(),
            icon: definition.icon.as_deref(),
            icon_url: definition.icon_url.as_deref(),
        };
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/achievements",
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

    /// `PATCH /integrations/{slug}/achievements/{key}` (#324/#325, closed
    /// out by #744) — updates name/description/schema/icon (bumping
    /// `version` when the definition itself changed) and/or retires the
    /// achievement (one-way — `update.retired = Some(false)` never
    /// un-retires). Every field in `update` left `None` is left untouched.
    pub async fn update_achievement_definition(
        &self,
        slug: &str,
        key: &str,
        update: ClaimDefinitionUpdate,
    ) -> Result<AchievementDefinition, SdkError> {
        let headers = self.definition_auth_headers(slug).await?;
        let body = UpdateClaimDefinitionRequest {
            name: update.name.as_deref(),
            description: update.description.as_deref(),
            schema: update.schema.as_deref(),
            icon: update.icon.as_deref(),
            icon_url: update.icon_url.as_deref(),
            retired: update.retired,
        };
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .patch(format!(
                    "{}/integrations/{slug}/achievements/{key}",
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

    /// `POST /integrations/{slug}/milestones` (#324/#325, closed out by
    /// #744) — the App/Service equivalent of
    /// [`Session::create_achievement_definition`]; same shape, different
    /// route.
    pub async fn create_milestone_definition(
        &self,
        slug: &str,
        definition: NewClaimDefinition,
    ) -> Result<AchievementDefinition, SdkError> {
        let headers = self.definition_auth_headers(slug).await?;
        let body = CreateClaimDefinitionRequest {
            key: &definition.key,
            name: &definition.name,
            description: &definition.description,
            schema: definition.schema.as_deref(),
            icon: definition.icon.as_deref(),
            icon_url: definition.icon_url.as_deref(),
        };
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .post(format!(
                    "{}/integrations/{slug}/milestones",
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

    /// `PATCH /integrations/{slug}/milestones/{key}` (#324/#325, closed
    /// out by #744) — the App/Service equivalent of
    /// [`Session::update_achievement_definition`]; same shape, different
    /// route.
    pub async fn update_milestone_definition(
        &self,
        slug: &str,
        key: &str,
        update: ClaimDefinitionUpdate,
    ) -> Result<AchievementDefinition, SdkError> {
        let headers = self.definition_auth_headers(slug).await?;
        let body = UpdateClaimDefinitionRequest {
            name: update.name.as_deref(),
            description: update.description.as_deref(),
            schema: update.schema.as_deref(),
            icon: update.icon.as_deref(),
            icon_url: update.icon_url.as_deref(),
            retired: update.retired,
        };
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            let mut request = c
                .patch(format!(
                    "{}/integrations/{slug}/milestones/{key}",
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

    /// The milestone counterpart of [`Session::submit_achievement_issuance`]
    /// — identical two-proof shape, against
    /// `/integrations/{slug}/milestones/{key}/issue` instead. `category`
    /// must be this integrator's own actually-registered
    /// [`IntegratorCategory`] (`App` or `Service` — never `Game`, #324's
    /// category split, enforced server-side by
    /// `achievements::authenticate_owning_issuer`/`ClaimRoute::allows`):
    /// it's what builds the `<category>:<slug>` issuer prefix and
    /// `<category>:<slug>:milestone:<key>` achievement ref this key's
    /// signature covers, since (unlike achievement issuance) a milestone
    /// issuer is never assumed to be a `"game:"`.
    pub(crate) async fn submit_milestone_issuance(
        &self,
        key: &str,
        category: IntegratorCategory,
    ) -> Result<Uuid, SdkError> {
        let idempotency_key = Uuid::new_v4().to_string();
        crate::http::retry_write(&self.retry, || {
            self.attempt_milestone_issuance(key, category, &idempotency_key)
        })
        .await
    }

    async fn attempt_milestone_issuance(
        &self,
        key: &str,
        category: IntegratorCategory,
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
        let claim_kind = category.claim_kind();
        let issuer_ref = format!("{}:{slug}", category.as_str());
        let achievement = format!("{}:{slug}:milestone:{key}", category.as_str());
        let signing_bytes =
            attestation_signing_bytes(claim_kind, &issuer_ref, subject, &achievement);
        let signature = signing_key.sign(&signing_bytes);

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{}/milestones/{}/issue",
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

    /// The milestone counterpart of
    /// [`Session::submit_bulk_achievement_issuance`] — see
    /// [`Session::submit_milestone_issuance`] for why `category` is
    /// required here but not for achievement issuance.
    pub(crate) async fn submit_bulk_milestone_issuance(
        &self,
        keys: &[&str],
        category: IntegratorCategory,
    ) -> Result<Vec<BulkClaimOutcome>, SdkError> {
        let idempotency_key = Uuid::new_v4().to_string();
        crate::http::retry_write(&self.retry, || {
            self.attempt_bulk_milestone_issuance(keys, category, &idempotency_key)
        })
        .await
    }

    async fn attempt_bulk_milestone_issuance(
        &self,
        keys: &[&str],
        category: IntegratorCategory,
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
        let claim_kind = category.claim_kind();
        let issuer_ref = format!("{}:{slug}", category.as_str());
        let achievements: Vec<String> = keys
            .iter()
            .map(|key| format!("{}:{slug}:milestone:{key}", category.as_str()))
            .collect();
        let signing_bytes =
            bulk_attestation_signing_bytes(claim_kind, &issuer_ref, subject, &achievements);
        let signature = signing_key.sign(&signing_bytes);

        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!(
                "{}/integrations/{}/milestones/bulk-issue",
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

/// One achievement/milestone definition this integrator has published —
/// mirrors `crates/server/src/achievements.rs::AchievementDefinitionResponse`
/// at the wire level, shared by both achievement and milestone reads since
/// the server response shape is identical either way.
#[derive(Debug, Clone, Deserialize)]
pub struct AchievementDefinition {
    /// This definition's own namespaced id, e.g.
    /// `"game:<slug>:achievement:<key>"` / `"app:<slug>:milestone:<key>"`.
    pub id: String,
    /// The publishing integrator's id.
    pub integrator_id: Uuid,
    /// The short key this definition is issued/looked up by.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Display description.
    pub description: String,
    /// A `GlobalId` string pointing at a published Integrator Space schema,
    /// if this definition declares one.
    pub schema: Option<String>,
    /// Always populated — falls back to the server's own default icon when
    /// neither `icon` nor `icon_url` was set (issue #332).
    pub icon: String,
    /// Takes precedence over `icon` when present.
    pub icon_url: Option<String>,
    /// Bumps whenever the definition's own content changes.
    pub version: i32,
    /// When this definition was first created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When this definition was last changed (content or retirement).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// One-way — never flips back to `false` once `true`.
    pub retired: bool,
    /// When this definition was retired, if it has been.
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}

/// The fields [`Session::create_achievement_definition`]/
/// [`Session::create_milestone_definition`] need — see
/// `crates/server/src/achievements.rs::CreateAchievementDefinitionRequest`
/// for the exact server-side contract each field maps to.
#[derive(Debug, Clone, Default)]
pub struct NewClaimDefinition {
    /// Lowercase `[a-z0-9_]`, 2-128 characters.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Display description.
    pub description: String,
    /// A `GlobalId` string pointing at a published Integrator Space
    /// schema, if any.
    pub schema: Option<String>,
    /// One of the server's built-in icon names; `None` falls back to the
    /// default icon at read time.
    pub icon: Option<String>,
    /// An integrator-hosted `http`/`https` image URL, taking precedence
    /// over `icon` when present.
    pub icon_url: Option<String>,
}

#[derive(Serialize)]
struct CreateClaimDefinitionRequest<'a> {
    key: &'a str,
    name: &'a str,
    description: &'a str,
    schema: Option<&'a str>,
    icon: Option<&'a str>,
    icon_url: Option<&'a str>,
}

/// The fields [`Session::update_achievement_definition`]/
/// [`Session::update_milestone_definition`] accept — every field left
/// `None` leaves that part of the definition untouched, same "absent means
/// untouched" convention the server side documents. `retired = Some(true)`
/// retires the definition; there is no un-retire.
#[derive(Debug, Clone, Default)]
pub struct ClaimDefinitionUpdate {
    /// New display name, if changing.
    pub name: Option<String>,
    /// New display description, if changing.
    pub description: Option<String>,
    /// New schema `GlobalId` string, if changing.
    pub schema: Option<String>,
    /// New built-in icon name, if changing.
    pub icon: Option<String>,
    /// New integrator-hosted icon URL, if changing.
    pub icon_url: Option<String>,
    /// `Some(true)` retires the definition. There is no un-retire.
    pub retired: Option<bool>,
}

#[derive(Serialize)]
struct UpdateClaimDefinitionRequest<'a> {
    name: Option<&'a str>,
    description: Option<&'a str>,
    schema: Option<&'a str>,
    icon: Option<&'a str>,
    icon_url: Option<&'a str>,
    retired: Option<bool>,
}

impl AvalonClient {
    /// `GET /attestations/{id}` (#33, closed out by #744) — public,
    /// unauthenticated single-attestation read, reusing
    /// [`VerifiedAttestation`] since the server's response shape is
    /// identical to `GET /me/achievements`'s per-item shape. Same "facts,
    /// not a per-consumer verdict" posture: no `recognition` field, see
    /// this module's doc comment.
    pub async fn get_attestation(&self, id: Uuid) -> Result<VerifiedAttestation, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/attestations/{id}", self.config.server_url))
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

    /// `GET /integrations/{slug}/achievements` (#324/#325, closed out by
    /// #744) — every achievement `slug` has defined, including retired
    /// ones (`retired: true`, never hidden — a retired definition's past
    /// attestations still need somewhere to point). Public, unauthenticated.
    pub async fn list_achievement_definitions(
        &self,
        slug: &str,
    ) -> Result<Vec<AchievementDefinition>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/achievements",
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

    /// `GET /integrations/{slug}/milestones` (#324/#325, closed out by
    /// #744) — the App/Service equivalent of
    /// [`AvalonClient::list_achievement_definitions`].
    pub async fn list_milestone_definitions(
        &self,
        slug: &str,
    ) -> Result<Vec<AchievementDefinition>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!(
                "{}/integrations/{slug}/milestones",
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
mod definition_tests {
    use super::*;

    #[test]
    fn achievement_definition_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "id": "game:ashen-realms:achievement:first_blood",
            "integrator_id": Uuid::nil(),
            "key": "first_blood",
            "name": "First Blood",
            "description": "Win your first match",
            "schema": null,
            "icon": "trophy",
            "icon_url": null,
            "version": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "retired": false,
            "retired_at": null,
        });
        let definition: AchievementDefinition = serde_json::from_value(raw).unwrap();
        assert_eq!(definition.key, "first_blood");
        assert_eq!(definition.version, 1);
        assert!(!definition.retired);
    }

    #[test]
    fn verified_attestation_deserializes_from_the_documented_wire_shape() {
        let raw = serde_json::json!({
            "id": Uuid::nil(),
            "issuer": "game:ashen-realms",
            "subject": Uuid::nil(),
            "achievement": "game:ashen-realms:achievement:first_blood",
            "issued_at": "2026-01-01T00:00:00Z",
            "authenticity": { "status": "authentic", "key_id": Uuid::nil() },
            "validity": { "status": "valid" },
            "history": [],
        });
        let attestation: VerifiedAttestation = serde_json::from_value(raw).unwrap();
        assert!(matches!(
            attestation.authenticity,
            Authenticity::Authentic { .. }
        ));
        assert!(matches!(attestation.validity, Validity::Valid));
    }
}
