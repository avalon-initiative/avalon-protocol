//! Per-network issuer registration gate: network isolation is enforced by admission, not by
//! binding `network_id` into attestation/event signatures
//! (`crates/protocol/src/achievements.rs`'s `attestation_signing_bytes`
//! deliberately excludes it — a signature verifies identically on every
//! network). A signature that verifies against an issuer's registered key
//! (`crate::integrators`'s `issuer_keys`, custody/rotation history) still
//! isn't enough to write on a given network unless that same public key
//! has also been admitted here — authenticity and network-admission are
//! two independent checks, composing with the authenticity/validity/
//! recognition split rather than replacing any part of it.
//!
//! Two distinct tables, on purpose — see migration 0059's own comment:
//! `issuer_network_registrations` (this module) is a flat admission list
//! keyed on raw public key bytes, unrelated to `issuer_keys`'s
//! integrator-scoped key custody/rotation history. The registration wire
//! shape (`issuer_pubkey`/`issuer_ref`, no `integrator_id`/`key_id`) is
//! deliberately decoupled from the `integrators`/`issuer_keys` tables.
//!
//! Admission policy differs by network tier ([`NetworkTier`]), not by
//! anything in the registration handler itself, which is identical and
//! unreviewed on every tier: `dev`/`int` auto-register a key the first
//! time it's seen on a valid signed write (see [`ensure_issuer_registered`],
//! called from the achievement-issuance write path); `mainnet` never does
//! — an unregistered key's write is rejected outright.

use avalon_protocol::event_payloads::IssuerRegisteredPayload;
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
use axum::extract::State;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::outbox;
use crate::state::AppState;

const REGISTRATION_CHALLENGE_TTL_MINUTES: i64 = 5;
const REGISTRATION_NONCE_BYTES: usize = 32;

/// The three deployment tiers `docs/trusted-networks.json`'s `network_id`
/// prefix convention already establishes (`avalon-dev-<name>` /
/// `avalon-int-<name>` / `avalon-mainnet-N`) — see
/// `docs/architecture/network-trust-anchors.md`. An unrecognized prefix (a
/// `network_id` following none of these conventions) is treated as
/// [`NetworkTier::Mainnet`], the strictest tier, rather than guessed as
/// `Dev` — failing closed on an unrecognized `network_id`, never silently
/// permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkTier {
    Dev,
    Int,
    Mainnet,
}

impl NetworkTier {
    pub(crate) fn for_network_id(network_id: &str) -> Self {
        if network_id.starts_with("avalon-dev-") {
            NetworkTier::Dev
        } else if network_id.starts_with("avalon-int-") {
            NetworkTier::Int
        } else {
            NetworkTier::Mainnet
        }
    }

    /// Whether an unregistered key's first valid signed write should be
    /// silently admitted rather than rejected — true on dev/int, false on
    /// mainnet. See module doc comment.
    fn auto_registers(self) -> bool {
        matches!(self, NetworkTier::Dev | NetworkTier::Int)
    }
}

#[derive(Deserialize, ToSchema)]
pub struct RegistrationChallengeRequest {
    /// Standard-base64-encoded Ed25519 public key bytes.
    pub issuer_pubkey: String,
}

#[derive(Serialize, ToSchema)]
pub struct RegistrationChallengeResponse {
    pub challenge_id: Uuid,
    /// Standard-base64-encoded random nonce — signed (as part of a larger
    /// canonical message, see [`proof_of_possession_message`]) and echoed
    /// back via [`register_issuer`].
    pub nonce: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: OffsetDateTime,
}

/// `POST /issuers/registration-challenge` — issues a short-lived,
/// single-use nonce, mirroring `integrators::create_integrator_challenge`'s
/// shape. No auth: obtaining a challenge proves nothing by itself, only
/// actually signing it (see [`register_issuer`]) does.
#[utoipa::path(
    post,
    path = "/issuers/registration-challenge",
    tag = "issuers",
    request_body = RegistrationChallengeRequest,
    responses((status = 200, body = RegistrationChallengeResponse)),
)]
pub async fn create_registration_challenge(
    State(state): State<AppState>,
    Json(body): Json<RegistrationChallengeRequest>,
) -> Result<Json<RegistrationChallengeResponse>, AppError> {
    let issuer_pubkey = BASE64
        .decode(&body.issuer_pubkey)
        .map_err(|_| AppError::InvalidProofOfPossession)?;

    let mut nonce = [0u8; REGISTRATION_NONCE_BYTES];
    rand::rng().fill_bytes(&mut nonce);
    let challenge_id = Uuid::new_v4();
    let expires_at =
        OffsetDateTime::now_utc() + time::Duration::minutes(REGISTRATION_CHALLENGE_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO issuer_registration_challenges (id, issuer_pubkey, nonce, expires_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(challenge_id)
    .bind(&issuer_pubkey)
    .bind(nonce.as_slice())
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(RegistrationChallengeResponse {
        challenge_id,
        nonce: BASE64.encode(nonce),
        expires_at,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct RegisterIssuerRequest {
    pub issuer_pubkey: String,
    pub issuer_ref: String,
    pub declared_network_id: String,
    pub challenge_id: Uuid,
    pub proof_of_possession_signature: String,
}

#[derive(Serialize, ToSchema)]
pub struct IssuerRegistrationResponse {
    pub issuer_ref: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub registered_at: OffsetDateTime,
}

/// The canonical proof-of-possession message — same `"avalon:<kind>:v1:..."`
/// convention `attestation_signing_bytes` already establishes. Binds
/// `issuer_ref` and the declared network into what's actually signed, not
/// just the raw nonce, so a challenge issued for one issuer_ref/network
/// pair can't be replayed to register a different one.
fn proof_of_possession_message(
    issuer_ref: &str,
    declared_network_id: &str,
    nonce: &[u8],
) -> Vec<u8> {
    format!(
        "avalon:issuer.registered:v1:{issuer_ref}:{declared_network_id}:{}",
        BASE64.encode(nonce)
    )
    .into_bytes()
}

/// `POST /issuers/register` — explicit, self-service, never
/// reviewed/approved on any tier (see module doc comment); the "gate" is
/// which networks admit an unregistered key implicitly, not who may call
/// this endpoint. Idempotent: registering an already-registered key
/// updates its `issuer_ref` rather than erroring, matching this
/// endpoint's "admission, not gatekeeping" posture.
#[utoipa::path(
    post,
    path = "/issuers/register",
    tag = "issuers",
    request_body = RegisterIssuerRequest,
    responses((status = 200, body = IssuerRegistrationResponse)),
)]
pub async fn register_issuer(
    State(state): State<AppState>,
    Json(body): Json<RegisterIssuerRequest>,
) -> Result<Json<IssuerRegistrationResponse>, AppError> {
    // Belt-and-suspenders alongside the SDK/CLI-side check: a
    // request declaring a network other than this server's own is a
    // client-side mistake, not an authentication failure.
    if body.declared_network_id != state.chain.network_id() {
        return Err(AppError::DeclaredNetworkMismatch);
    }

    let issuer_pubkey = BASE64
        .decode(&body.issuer_pubkey)
        .map_err(|_| AppError::InvalidProofOfPossession)?;
    let signature_bytes = BASE64
        .decode(&body.proof_of_possession_signature)
        .map_err(|_| AppError::InvalidProofOfPossession)?;

    let challenge_row = sqlx::query(
        "DELETE FROM issuer_registration_challenges WHERE id = $1 AND issuer_pubkey = $2 \
         RETURNING nonce, expires_at",
    )
    .bind(body.challenge_id)
    .bind(&issuer_pubkey)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::IssuerRegistrationChallengeNotFound)?;
    let expires_at: OffsetDateTime = challenge_row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::IssuerRegistrationChallengeExpired);
    }
    let nonce: Vec<u8> = challenge_row.try_get("nonce")?;

    let message = proof_of_possession_message(&body.issuer_ref, &body.declared_network_id, &nonce);
    if !verify_event_signature(&issuer_pubkey, &message, &signature_bytes) {
        return Err(AppError::InvalidProofOfPossession);
    }

    let registered_at = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;
    upsert_registration(
        &mut tx,
        &issuer_pubkey,
        &body.issuer_ref,
        registered_at,
        false,
    )
    .await?;
    enqueue_registration_event(
        &mut tx,
        &body.issuer_ref,
        state.chain.network_id(),
        registered_at,
    )
    .await?;
    tx.commit().await?;

    Ok(Json(IssuerRegistrationResponse {
        issuer_ref: body.issuer_ref,
        registered_at,
    }))
}

async fn upsert_registration(
    tx: &mut Transaction<'_, Postgres>,
    issuer_pubkey: &[u8],
    issuer_ref: &str,
    registered_at: OffsetDateTime,
    auto_registered: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO issuer_network_registrations (issuer_pubkey, issuer_ref, registered_at, auto_registered) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (issuer_pubkey) DO UPDATE SET issuer_ref = EXCLUDED.issuer_ref",
    )
    .bind(issuer_pubkey)
    .bind(issuer_ref)
    .bind(registered_at)
    .bind(auto_registered)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn enqueue_registration_event(
    tx: &mut Transaction<'_, Postgres>,
    issuer_ref: &str,
    network_id: &str,
    registered_at: OffsetDateTime,
) -> Result<(), AppError> {
    let (namespace, slug) = issuer_ref.split_once(':').unwrap_or(("issuer", issuer_ref));
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IssuerRegistered
            .as_str()
            .to_string(),
        issuer: GlobalId::new(namespace, slug, "self", "issuer_registered"),
        subject: GlobalId::new("network", network_id, "issuer", "registered"),
        payload: serde_json::to_value(IssuerRegisteredPayload {
            issuer_ref: issuer_ref.to_string(),
            network_id: network_id.to_string(),
            registered_at,
        })
        .expect("IssuerRegisteredPayload should serialize"),
        timestamp: registered_at,
        version: 1,
    };
    outbox::enqueue(tx, &event).await?;
    Ok(())
}

/// Write-path gate: called from wherever an attestation/event
/// signature has already verified authentic against the issuer's key
/// history (`crate::achievements`'s `verify_authenticity` call) — see
/// module doc comment for why this is a second, independent check. Must
/// run inside the same transaction as the write it's gating, so a
/// dev/int auto-registration and the write it admits commit atomically
/// together — never a registration that "succeeded" while the write it
/// was supposed to unblock then failed for an unrelated reason.
pub(crate) async fn ensure_issuer_registered(
    tx: &mut Transaction<'_, Postgres>,
    network_id: &str,
    issuer_pubkey: &[u8],
    issuer_ref: &str,
) -> Result<(), AppError> {
    let already_registered = sqlx::query(
        "SELECT 1 AS present FROM issuer_network_registrations WHERE issuer_pubkey = $1",
    )
    .bind(issuer_pubkey)
    .fetch_optional(&mut **tx)
    .await?
    .is_some();

    if already_registered {
        return Ok(());
    }

    if NetworkTier::for_network_id(network_id).auto_registers() {
        upsert_registration(
            tx,
            issuer_pubkey,
            issuer_ref,
            OffsetDateTime::now_utc(),
            true,
        )
        .await?;
        Ok(())
    } else {
        Err(AppError::IssuerNotRegisteredOnNetwork)
    }
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks on
    //! `NetworkTier::for_network_id`/`auto_registers` only. The full
    //! registration + write-path-gate flow is covered by
    //! `crates/server/tests/issuer_registration.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn dev_and_int_network_ids_auto_register() {
        assert!(NetworkTier::for_network_id("avalon-dev-local").auto_registers());
        assert!(NetworkTier::for_network_id("avalon-int-1").auto_registers());
    }

    #[test]
    fn mainnet_network_ids_never_auto_register() {
        assert!(!NetworkTier::for_network_id("avalon-mainnet-1").auto_registers());
    }

    #[test]
    fn an_unrecognized_network_id_fails_closed_to_mainnet_strictness() {
        assert_eq!(
            NetworkTier::for_network_id("something-else"),
            NetworkTier::Mainnet
        );
        assert!(!NetworkTier::for_network_id("something-else").auto_registers());
    }
}
