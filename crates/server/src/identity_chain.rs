//! Server-side entry points for per-identity event chains: assigning a
//! chain position to an event an identity authors, and refusing operations
//! that depend on knowing which key controls a forked identity.

use avalon_indexer::identity_chain_store;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::IdentityId;
use sqlx::{PgExecutor, Postgres, Transaction};

use crate::error::AppError;

/// Assigns `event` its place in its identity's chain within `tx`, so the
/// chain state, the write and the outbox entry commit together. A no-op for
/// kinds that are not chained.
pub async fn assign(
    tx: &mut Transaction<'_, Postgres>,
    event: &mut ProtocolEvent,
) -> Result<(), sqlx::Error> {
    identity_chain_store::assign_local(tx, event).await
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
) -> Result<(), AppError> {
    let head = identity_chain_store::lock_head(tx, identity_id).await?;
    if head.forked_at_seq.is_some() {
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
