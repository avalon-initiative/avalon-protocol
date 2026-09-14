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
//! recognition verdict on the caller's behalf either; a game that wants
//! one filters [`VerifiedAttestation`]s through its own policy.
//!
//! **Issuing** (`Session::issue_achievement`) needs this integrator's own
//! signing key — the server never sees it, only a detached signature
//! (`AvalonConfig::game_slug`/`signing_key`). Two independent proofs go
//! out, mirroring every other issuer-credentialed endpoint in this repo
//! (`docs/architecture/games-and-issuers.md`): an ephemeral
//! challenge-response proving *this key* is making the HTTP call right now
//! (`POST /games/{slug}/challenge`), and a separate signature embedded in
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
    Authentic { key_id: String },
    NotAuthentic { reason: String },
}

/// Mirrors `avalon_protocol::achievements::Validity` at the wire level.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Validity {
    Valid,
    Invalid { reason: String },
}

/// One entry in an attestation's history (`"issued"`, plus `"revoked"` if
/// applicable, #85) — see `crate::attestations::AttestationHistoryEntry`
/// server-side.
#[derive(Debug, Clone, Deserialize)]
pub struct AttestationHistoryEntry {
    pub event: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
}

/// One attestation from the caller's own history, with its computed
/// authenticity and validity attached — deliberately no `recognition`
/// field, see module doc comment.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifiedAttestation {
    pub id: Uuid,
    pub issuer: String,
    pub subject: Uuid,
    pub achievement: String,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    pub authenticity: Authenticity,
    pub validity: Validity,
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

impl Session {
    pub(crate) async fn fetch_achievements(&self) -> Result<Vec<VerifiedAttestation>, SdkError> {
        let response = self
            .http
            .get(format!("{}/me/achievements", self.server_url))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(SdkError::ServerError(response.status()));
        }

        Ok(response.json().await?)
    }

    pub(crate) async fn submit_achievement_issuance(&self, key: &str) -> Result<Uuid, SdkError> {
        let slug = self
            .game_slug
            .as_deref()
            .ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key_bytes = self.signing_key.ok_or(SdkError::MissingIssuerCredentials)?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);
        let key_id: Uuid = self
            .integrator_key_id
            .parse()
            .map_err(|_| SdkError::MissingIssuerCredentials)?;

        // Proof one: this key is making this HTTP call, right now.
        let challenge: ChallengeResponse = self
            .http
            .post(format!("{}/games/{}/challenge", self.server_url, slug))
            .send()
            .await?
            .json()
            .await?;
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

        let response = self
            .http
            .post(format!(
                "{}/games/{}/achievements/{}/issue",
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
            .json(&IssueRequest {
                key_id,
                signature: BASE64.encode(signature.to_bytes()),
            })
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(SdkError::ServerError(response.status()));
        }

        let body: IssueResponse = response.json().await?;
        Ok(body.id)
    }
}
