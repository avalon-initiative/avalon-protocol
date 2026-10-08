//! Server-side entry points for per-identity event chains: assigning a
//! chain position to an event an identity authors, and refusing operations
//! that depend on knowing which key controls a forked identity.

use avalon_indexer::identity_chain_store;
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::{signer_of, verify_author_signature};
use avalon_protocol::ids::IdentityId;
use serde::Deserialize;
use sqlx::{PgExecutor, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// What a client signs to author a chained event: the event identity and time it chose, the chain
/// position it signed (the head it saw plus one) and its key's Ed25519 signature over
/// `chain_event_signature_bytes` of the event the endpoint builds.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ChainEventSignature {
    /// Id of the event the endpoint will author; chosen by the signer.
    pub event_id: Uuid,
    /// Event time, at microsecond precision, within five minutes of the node's clock.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub timestamp: OffsetDateTime,
    /// Chain position: the identity's head `seq` plus one.
    pub seq: u64,
    /// The head's event hash as 64 lowercase hex characters; absent for the first chained event.
    pub prev_hash: Option<String>,
    /// The active signing key of the author that signed.
    pub signing_key_id: Uuid,
    /// Base64 Ed25519 signature.
    pub signature: String,
}

/// A request body that only carries the author's signature of the event the endpoint authors.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SignedAction {
    pub chain_event: ChainEventSignature,
}

/// Places a client-signed event in its owner's chain within `tx`, so the chain state, the write and
/// the outbox entry commit together: the event takes the client's id, time, position and signature,
/// the signature is verified against an active key of the author (or `new_key`, for the key a
/// recovery introduces), and the position must be exactly the locked head plus one.
pub async fn place_signed(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    event: &mut ProtocolEvent,
    sig: &ChainEventSignature,
    new_key: Option<[u8; 32]>,
) -> Result<(), AppError> {
    let owner = avalon_protocol::identity_chain_wire::chain_owner(event)
        .ok_or(AppError::InvalidChainPosition)?;
    let prev_hash = parse_prev_hash(sig.prev_hash.as_deref())?;
    let bounds = avalon_protocol::identity_chain::ClockSkewBounds::default();
    avalon_protocol::identity_chain::clamp_timestamp(
        avalon_protocol::identity_chain_wire::truncate_to_micros(sig.timestamp),
        OffsetDateTime::now_utc(),
        &bounds,
    )
    .map_err(|_| AppError::InvalidAuthorSignature)?;
    event.id = sig.event_id;
    event.timestamp = avalon_protocol::identity_chain_wire::truncate_to_micros(sig.timestamp);
    event.identity_chain = Some(
        IdentityChainPosition::current(sig.seq, prev_hash.map(hex::encode))
            .signed(sig.signing_key_id, sig.signature.clone()),
    );

    let key = match new_key {
        Some(key) => key,
        None => {
            let signer = signer_of(event).ok_or(AppError::InvalidAuthorSignature)?;
            let raw: Vec<u8> = sqlx::query_scalar(
                "SELECT public_key FROM identity_signing_keys \
                 WHERE id = $1 AND identity_id = $2 AND revoked_at IS NULL",
            )
            .bind(sig.signing_key_id)
            .bind(signer)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(AppError::InvalidAuthorSignature)?;
            <[u8; 32]>::try_from(raw).map_err(|_| AppError::InvalidAuthorSignature)?
        }
    };
    if !matches!(
        verify_author_signature(event, state.chain.network_id(), &key),
        Ok(true)
    ) {
        return Err(AppError::InvalidAuthorSignature);
    }
    let resolves_fork = event.kind == "identity.recovered";
    require_signed_position(tx, owner, sig.seq, sig.prev_hash.as_deref(), resolves_fork).await?;
    identity_chain_store::record_local(tx, event).await?;
    Ok(())
}

/// The head of an identity's event chain: the position the next signed event must extend.
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct ChainHeadResponse {
    /// `seq` of the head event; 0 before the first chained event.
    pub seq: u64,
    /// The head event's hash as 64 lowercase hex characters; absent before the first chained event.
    pub head_hash: Option<String>,
    /// Set while the chain is forked; only `identity.recovered` extends it then.
    pub forked_at_seq: Option<u64>,
}

/// `GET /identities/{id}/chain-head` — where the next event of the identity's chain goes: sign
/// `seq + 1` with `prev_hash = head_hash`.
#[utoipa::path(
    get,
    path = "/identities/{id}/chain-head",
    tag = "identities",
    params(("id" = IdentityId, Path)),
    responses((status = 200, description = "The chain head", body = ChainHeadResponse)),
)]
pub async fn chain_head(
    axum::extract::State(state): axum::extract::State<AppState>,
    crate::error::IdPath(identity_id): crate::error::IdPath<IdentityId>,
) -> Result<axum::Json<ChainHeadResponse>, AppError> {
    let head = identity_chain_store::state(&state.pool, identity_id).await?;
    Ok(axum::Json(match head {
        Some(h) => ChainHeadResponse {
            seq: h.seq as u64,
            head_hash: h.head_hash,
            forked_at_seq: h.forked_at_seq.map(|s| s as u64),
        },
        None => ChainHeadResponse {
            seq: 0,
            head_hash: None,
            forked_at_seq: None,
        },
    }))
}

/// Places an owner-signed key event (whose signature the caller verified) at the position it
/// signed, which the caller checked with [`require_signed_position`].
pub async fn place_key_event(
    tx: &mut Transaction<'_, Postgres>,
    event: &mut ProtocolEvent,
    seq: u64,
    prev_hash: Option<&str>,
) -> Result<(), sqlx::Error> {
    event.timestamp = avalon_protocol::identity_chain_wire::truncate_to_micros(event.timestamp);
    event.identity_chain = Some(IdentityChainPosition::current(
        seq,
        prev_hash.map(str::to_string),
    ));
    identity_chain_store::record_local(tx, event).await?;
    Ok(())
}

/// Refuses with [`AppError::IdentityChainForked`] while `identity_id`'s
/// chain is forked.
pub async fn ensure_not_forked<'e>(
    exec: impl PgExecutor<'e>,
    identity_id: IdentityId,
) -> Result<(), AppError> {
    if identity_chain_store::is_forked(exec, identity_id).await? {
        return Err(AppError::IdentityChainForked);
    }
    Ok(())
}

/// The position a signer covered, locked against the identity's current head: refuses with
/// [`AppError::IdentityChainPositionStale`] (carrying the head to re-sign against) unless it is
/// exactly the next position, so the event assigned afterwards lands where it was signed.
pub async fn require_signed_position(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
    seq: u64,
    prev_hash: Option<&str>,
    resolves_fork: bool,
) -> Result<(), AppError> {
    let head = identity_chain_store::lock_head(tx, identity_id).await?;
    if head.forked_at_seq.is_some() && !resolves_fork {
        return Err(AppError::IdentityChainForked);
    }
    if seq != head.seq as u64 + 1 || prev_hash != head.head_hash.as_deref() {
        return Err(AppError::IdentityChainPositionStale {
            head_seq: head.seq as u64,
            head_hash: head.head_hash,
        });
    }
    Ok(())
}

/// Decodes a request's hex `prev_hash` (`None` at genesis) to the 32 bytes the signature covers.
pub fn parse_prev_hash(prev_hash: Option<&str>) -> Result<Option<[u8; 32]>, AppError> {
    prev_hash
        .map(|h| {
            avalon_protocol::identity_chain_wire::parse_hash(h)
                .filter(|_| h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                .ok_or(AppError::InvalidChainPosition)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prev_hash_must_be_64_lowercase_hex() {
        assert_eq!(parse_prev_hash(None).unwrap(), None);
        assert_eq!(
            parse_prev_hash(Some(&"ab".repeat(32))).unwrap(),
            Some([0xab; 32])
        );
        for bad in [
            "",
            "ab",
            &"AB".repeat(32),
            &"zz".repeat(32),
            &"ab".repeat(33),
        ] {
            assert!(matches!(
                parse_prev_hash(Some(bad)),
                Err(AppError::InvalidChainPosition)
            ));
        }
    }

    #[tokio::test]
    async fn a_stale_position_answers_409_with_the_head_to_sign_against() {
        use axum::response::IntoResponse as _;
        let response = AppError::IdentityChainPositionStale {
            head_seq: 4,
            head_hash: Some("ab".repeat(32)),
        }
        .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "IDENTITY_CHAIN_POSITION_STALE");
        assert_eq!(body["head_seq"], 4);
        assert_eq!(body["head_hash"], "ab".repeat(32));
    }
}
