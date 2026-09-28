//! Verification, pinning and mirroring bounds for self-certifying
//! (`node:<sha256-of-key>`) shards.
//!
//! A shard id fixes the key that signs its tree heads, so a verifier needs only
//! the key presented alongside a head: it must hash to the id and must have
//! signed the head. No registry and no authority is consulted. The first key
//! that passes is pinned; later resolution reads the pin.
//!
//! The caps here bound how much a node is willing to mirror. They never change
//! what counts as a valid head.

use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::shard_identity::{is_self_certifying, resolve_self_certifying_key};
use ed25519_dalek::VerifyingKey;
use sqlx::{PgPool, Row};

const DEFAULT_MAX_SHARDS: i64 = 32;
const DEFAULT_MAX_ENTRIES_PER_SHARD: i64 = 100_000;

/// Mirroring limits for self-certifying shards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorBounds {
    /// Most distinct self-certifying shards this node will pin and mirror.
    pub max_shards: i64,
    /// Largest `tree_size` this node will mirror for one such shard.
    pub max_entries_per_shard: i64,
}

impl Default for MirrorBounds {
    fn default() -> Self {
        Self {
            max_shards: DEFAULT_MAX_SHARDS,
            max_entries_per_shard: DEFAULT_MAX_ENTRIES_PER_SHARD,
        }
    }
}

impl MirrorBounds {
    /// Reads `AVALON_MIRROR_MAX_SELF_CERTIFYING_SHARDS` and
    /// `AVALON_MIRROR_MAX_SELF_CERTIFYING_ENTRIES`, falling back to the defaults.
    pub fn from_env() -> Self {
        let read = |name: &str, default: i64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|v| *v >= 0)
                .unwrap_or(default)
        };
        Self {
            max_shards: read(
                "AVALON_MIRROR_MAX_SELF_CERTIFYING_SHARDS",
                DEFAULT_MAX_SHARDS,
            ),
            max_entries_per_shard: read(
                "AVALON_MIRROR_MAX_SELF_CERTIFYING_ENTRIES",
                DEFAULT_MAX_ENTRIES_PER_SHARD,
            ),
        }
    }
}

/// Why a head of a self-certifying shard was not accepted for mirroring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// No key was presented and none is pinned.
    NoKey,
    /// The presented key is malformed or does not hash to the shard id.
    KeyDoesNotMatchId,
    /// The key matches the id but did not sign the head.
    BadSignature,
    /// The shard's `tree_size` exceeds the per-shard entry cap.
    TooLarge,
    /// This node already pins its maximum number of self-certifying shards.
    ShardCapReached,
}

/// Decodes a hex-encoded 32-byte Ed25519 public key.
pub fn parse_public_key(hex_key: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(hex_key).ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// The key a head of `shard_id` must verify against: the pinned key when there
/// is one, otherwise the presented key. Either way it must hash to the id, so a
/// key differing from the pin can never pass.
pub fn select_key(
    shard_id: &str,
    pinned: Option<VerifyingKey>,
    presented_hex: Option<&str>,
) -> Result<VerifyingKey, Rejection> {
    let candidate = match (pinned, presented_hex) {
        (Some(pinned), Some(hex_key)) => match parse_public_key(hex_key) {
            Some(presented) if presented == pinned => pinned,
            _ => return Err(Rejection::KeyDoesNotMatchId),
        },
        (Some(pinned), None) => pinned,
        (None, Some(hex_key)) => parse_public_key(hex_key).ok_or(Rejection::KeyDoesNotMatchId)?,
        (None, None) => return Err(Rejection::NoKey),
    };
    resolve_self_certifying_key(shard_id, &candidate).ok_or(Rejection::KeyDoesNotMatchId)
}

/// Checks that `key` signed `head` and that the shard fits `bounds`.
pub fn check_head(
    key: &VerifyingKey,
    head: &CosignedTreeHead,
    bounds: &MirrorBounds,
) -> Result<(), Rejection> {
    if !avalon_protocol::sth::verify_tree_head(key, &head.sth) {
        return Err(Rejection::BadSignature);
    }
    if head.sth.tree_size > bounds.max_entries_per_shard {
        return Err(Rejection::TooLarge);
    }
    Ok(())
}

/// The pinned key of `shard_id`, if this node has verified one.
pub async fn pinned_key(pool: &PgPool, shard_id: &str) -> Option<VerifyingKey> {
    if !is_self_certifying(shard_id) {
        return None;
    }
    let row = sqlx::query("SELECT public_key FROM self_certifying_shard_keys WHERE shard_id = $1")
        .bind(shard_id)
        .fetch_optional(pool)
        .await
        .ok()??;
    let bytes: Vec<u8> = row.try_get("public_key").ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// Pins `key` for `shard_id` unless already pinned. A new pin is refused once
/// `bounds.max_shards` shards are pinned; an existing pin is always accepted.
pub async fn pin_key(
    pool: &PgPool,
    shard_id: &str,
    key: &VerifyingKey,
    bounds: &MirrorBounds,
) -> Result<(), Rejection> {
    if resolve_self_certifying_key(shard_id, key).is_none() {
        return Err(Rejection::KeyDoesNotMatchId);
    }
    if pinned_key(pool, shard_id).await.is_some() {
        return Ok(());
    }
    // The count and the insert share one statement so concurrent pins cannot
    // overshoot the cap.
    let inserted = sqlx::query(
        "INSERT INTO self_certifying_shard_keys (shard_id, public_key) \
         SELECT $1, $2 WHERE (SELECT count(*) FROM self_certifying_shard_keys) < $3 \
         ON CONFLICT (shard_id) DO NOTHING",
    )
    .bind(shard_id)
    .bind(key.as_bytes().as_slice())
    .bind(bounds.max_shards)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if inserted == 1 || pinned_key(pool, shard_id).await.is_some() {
        Ok(())
    } else {
        Err(Rejection::ShardCapReached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::shard_identity::derive_self_certifying_id;
    use avalon_protocol::sth::sign_tree_head;
    use ed25519_dalek::SigningKey;
    use time::OffsetDateTime;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn head(signing: &SigningKey, tree_size: i64) -> CosignedTreeHead {
        let sth = sign_tree_head(
            signing,
            "k",
            tree_size,
            &"ab".repeat(32),
            "net",
            OffsetDateTime::now_utc(),
        );
        CosignedTreeHead {
            sth,
            cosignatures: Vec::new(),
        }
    }

    fn hex_of(signing: &SigningKey) -> String {
        hex::encode(signing.verifying_key().to_bytes())
    }

    #[test]
    fn presented_key_that_hashes_to_the_id_is_selected() {
        let signing = key(1);
        let id = derive_self_certifying_id(&signing.verifying_key());
        let selected = select_key(&id, None, Some(&hex_of(&signing))).unwrap();
        assert_eq!(selected, signing.verifying_key());
    }

    #[test]
    fn presented_key_that_does_not_hash_to_the_id_is_rejected() {
        let id = derive_self_certifying_id(&key(1).verifying_key());
        assert_eq!(
            select_key(&id, None, Some(&hex_of(&key(2)))),
            Err(Rejection::KeyDoesNotMatchId)
        );
        assert_eq!(
            select_key(&id, None, Some("zz")),
            Err(Rejection::KeyDoesNotMatchId)
        );
        assert_eq!(select_key(&id, None, None), Err(Rejection::NoKey));
    }

    #[test]
    fn a_key_differing_from_the_pin_is_rejected() {
        let pinned = key(1);
        let id = derive_self_certifying_id(&pinned.verifying_key());
        assert_eq!(
            select_key(&id, Some(pinned.verifying_key()), Some(&hex_of(&key(2)))),
            Err(Rejection::KeyDoesNotMatchId)
        );
        assert!(select_key(&id, Some(pinned.verifying_key()), None).is_ok());
        assert!(select_key(&id, Some(pinned.verifying_key()), Some(&hex_of(&pinned))).is_ok());
    }

    #[test]
    fn head_verifies_with_only_the_certified_key() {
        let signing = key(3);
        let id = derive_self_certifying_id(&signing.verifying_key());
        let key = select_key(&id, None, Some(&hex_of(&signing))).unwrap();
        let bounds = MirrorBounds::default();
        assert_eq!(check_head(&key, &head(&signing, 5), &bounds), Ok(()));
        assert!(
            avalon_protocol::shard_identity::verify_self_certifying_tree_head(
                &id,
                &key,
                &head(&signing, 5).sth
            )
        );
    }

    #[test]
    fn head_signed_by_another_key_is_rejected() {
        let signing = key(3);
        let id = derive_self_certifying_id(&signing.verifying_key());
        let selected = select_key(&id, None, Some(&hex_of(&signing))).unwrap();
        assert_eq!(
            check_head(&selected, &head(&key(4), 5), &MirrorBounds::default()),
            Err(Rejection::BadSignature)
        );
    }

    #[test]
    fn oversized_shard_is_rejected() {
        let signing = key(5);
        let bounds = MirrorBounds {
            max_shards: 1,
            max_entries_per_shard: 10,
        };
        let verifying = signing.verifying_key();
        assert_eq!(check_head(&verifying, &head(&signing, 10), &bounds), Ok(()));
        assert_eq!(
            check_head(&verifying, &head(&signing, 11), &bounds),
            Err(Rejection::TooLarge)
        );
    }

    #[tokio::test]
    #[ignore]
    async fn pin_cap_and_key_pinning_hold_in_the_database() {
        {
            let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
            let pool = PgPool::connect(&database_url).await.unwrap();
            let existing: i64 =
                sqlx::query_scalar("SELECT count(*) FROM self_certifying_shard_keys")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let bounds = MirrorBounds {
                max_shards: existing + 2,
                max_entries_per_shard: 10,
            };
            let ids: Vec<_> = (10..13u8)
                .map(|s| (key(s), derive_self_certifying_id(&key(s).verifying_key())))
                .collect();
            assert!(
                pin_key(&pool, &ids[0].1, &ids[0].0.verifying_key(), &bounds)
                    .await
                    .is_ok()
            );
            assert!(
                pin_key(&pool, &ids[0].1, &ids[0].0.verifying_key(), &bounds)
                    .await
                    .is_ok()
            );
            assert!(
                pin_key(&pool, &ids[1].1, &ids[1].0.verifying_key(), &bounds)
                    .await
                    .is_ok()
            );
            assert_eq!(
                pin_key(&pool, &ids[2].1, &ids[2].0.verifying_key(), &bounds).await,
                Err(Rejection::ShardCapReached)
            );
            assert_eq!(
                pin_key(&pool, &ids[0].1, &ids[1].0.verifying_key(), &bounds).await,
                Err(Rejection::KeyDoesNotMatchId)
            );
            assert_eq!(
                pinned_key(&pool, &ids[0].1).await,
                Some(ids[0].0.verifying_key())
            );
            sqlx::query("DELETE FROM self_certifying_shard_keys WHERE shard_id = ANY($1)")
                .bind(ids.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>())
                .execute(&pool)
                .await
                .unwrap();
        }
    }
}
