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
const DEFAULT_MAX_SHARDS_PER_SOURCE: i64 = 4;
const DEFAULT_IDLE_SECS: i64 = 7 * 24 * 3600;
const DEFAULT_MAX_NEW_PER_TICK: usize = 4;

/// Serializes pin writes so the count-and-insert is atomic across connections.
const PIN_LOCK_ID: i64 = 0x5343_4b50_494e;

/// Mirroring limits for self-certifying shards. All of them bound how much this
/// node is willing to store and fetch; none changes what makes a head valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorBounds {
    /// Most distinct self-certifying shards this node keeps pinned.
    pub max_shards: i64,
    /// Largest `tree_size` this node will mirror for one such shard.
    pub max_entries_per_shard: i64,
    /// Most pinned shards served from one source URL.
    pub max_shards_per_source: i64,
    /// A pin unseen for this long may be evicted to make room.
    pub idle_secs: i64,
    /// Most previously unpinned shards examined per mirror tick.
    pub max_new_per_tick: usize,
}

impl Default for MirrorBounds {
    fn default() -> Self {
        Self {
            max_shards: DEFAULT_MAX_SHARDS,
            max_entries_per_shard: DEFAULT_MAX_ENTRIES_PER_SHARD,
            max_shards_per_source: DEFAULT_MAX_SHARDS_PER_SOURCE,
            idle_secs: DEFAULT_IDLE_SECS,
            max_new_per_tick: DEFAULT_MAX_NEW_PER_TICK,
        }
    }
}

impl MirrorBounds {
    /// Reads the `AVALON_MIRROR_*_SELF_CERTIFYING_*` variables, falling back to
    /// the defaults.
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
            max_shards_per_source: read(
                "AVALON_MIRROR_MAX_SELF_CERTIFYING_SHARDS_PER_SOURCE",
                DEFAULT_MAX_SHARDS_PER_SOURCE,
            ),
            idle_secs: read("AVALON_MIRROR_SELF_CERTIFYING_IDLE_SECS", DEFAULT_IDLE_SECS),
            max_new_per_tick: read(
                "AVALON_MIRROR_SELF_CERTIFYING_NEW_PER_TICK",
                DEFAULT_MAX_NEW_PER_TICK as i64,
            ) as usize,
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
    /// This node already pins its maximum number of self-certifying shards and
    /// none is idle enough to evict.
    ShardCapReached,
    /// This source URL already serves its maximum number of pinned shards.
    SourceCapReached,
    /// The head's network id is not this node's network.
    WrongNetwork,
    /// The database failed; the shard was neither pinned nor rejected on merit.
    Storage(String),
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

fn storage(e: sqlx::Error) -> Rejection {
    Rejection::Storage(e.to_string())
}

/// Whether a shard that is not pinned yet could be pinned now, without pinning
/// it: within the per-source cap, and either under the shard cap or with an idle
/// pin to evict. An already pinned shard is always admitted.
pub async fn can_admit(
    pool: &PgPool,
    shard_id: &str,
    source_url: &str,
    bounds: &MirrorBounds,
) -> Result<(), Rejection> {
    let row = sqlx::query(
        "SELECT \
           EXISTS (SELECT 1 FROM self_certifying_shard_keys WHERE shard_id = $1) AS pinned, \
           (SELECT count(*) FROM self_certifying_shard_keys) AS total, \
           (SELECT count(*) FROM self_certifying_shard_keys WHERE source_url = $2) AS from_source, \
           EXISTS (SELECT 1 FROM self_certifying_shard_keys \
                   WHERE last_seen_at < now() - make_interval(secs => $3::float8)) AS has_idle",
    )
    .bind(shard_id)
    .bind(source_url)
    .bind(bounds.idle_secs as f64)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    let get = |c: &str| row.try_get::<i64, _>(c).map_err(storage);
    if row.try_get::<bool, _>("pinned").map_err(storage)? {
        return Ok(());
    }
    if get("from_source")? >= bounds.max_shards_per_source {
        return Err(Rejection::SourceCapReached);
    }
    if get("total")? >= bounds.max_shards && !row.try_get::<bool, _>("has_idle").map_err(storage)? {
        return Err(Rejection::ShardCapReached);
    }
    Ok(())
}

/// Pins `key` for `shard_id`, or refreshes its last-seen time if already pinned.
/// A new pin respects the per-source and shard caps, evicting the least recently
/// seen idle pin when full; an active pin is never evicted. The check and write
/// run under an advisory lock, so the caps hold across concurrent callers.
pub async fn pin_key(
    pool: &PgPool,
    shard_id: &str,
    key: &VerifyingKey,
    source_url: &str,
    bounds: &MirrorBounds,
) -> Result<(), Rejection> {
    if resolve_self_certifying_key(shard_id, key).is_none() {
        return Err(Rejection::KeyDoesNotMatchId);
    }
    let mut tx = pool.begin().await.map_err(storage)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(PIN_LOCK_ID)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

    let existing: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT public_key FROM self_certifying_shard_keys WHERE shard_id = $1")
            .bind(shard_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
    if let Some(existing) = existing {
        if existing.as_slice() != key.as_bytes() {
            return Err(Rejection::KeyDoesNotMatchId);
        }
        sqlx::query(
            "UPDATE self_certifying_shard_keys SET last_seen_at = now() WHERE shard_id = $1",
        )
        .bind(shard_id)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        return tx.commit().await.map_err(storage);
    }

    let from_source: i64 =
        sqlx::query_scalar("SELECT count(*) FROM self_certifying_shard_keys WHERE source_url = $1")
            .bind(source_url)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
    if from_source >= bounds.max_shards_per_source {
        return Err(Rejection::SourceCapReached);
    }
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM self_certifying_shard_keys")
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
    if total >= bounds.max_shards {
        let evicted = sqlx::query(
            "DELETE FROM self_certifying_shard_keys WHERE shard_id = ( \
               SELECT shard_id FROM self_certifying_shard_keys \
               WHERE last_seen_at < now() - make_interval(secs => $1::float8) \
               ORDER BY last_seen_at LIMIT 1)",
        )
        .bind(bounds.idle_secs as f64)
        .execute(&mut *tx)
        .await
        .map_err(storage)?
        .rows_affected();
        if evicted == 0 {
            return Err(Rejection::ShardCapReached);
        }
    }
    sqlx::query(
        "INSERT INTO self_certifying_shard_keys (shard_id, public_key, source_url) VALUES ($1, $2, $3)",
    )
    .bind(shard_id)
    .bind(key.as_bytes().as_slice())
    .bind(source_url)
    .execute(&mut *tx)
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)
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
            max_entries_per_shard: 10,
            ..MirrorBounds::default()
        };
        let verifying = signing.verifying_key();
        assert_eq!(check_head(&verifying, &head(&signing, 10), &bounds), Ok(()));
        assert_eq!(
            check_head(&verifying, &head(&signing, 11), &bounds),
            Err(Rejection::TooLarge)
        );
    }

    // These tests size the cap from the table's global count, so they run one at a time.
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn live_pool() -> PgPool {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        PgPool::connect(&database_url).await.unwrap()
    }

    async fn total(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM self_certifying_shard_keys")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn fresh(n: u8) -> (SigningKey, String) {
        let mut seed = [n; 32];
        seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let signing = SigningKey::from_bytes(&seed);
        let id = derive_self_certifying_id(&signing.verifying_key());
        (signing, id)
    }

    async fn cleanup(pool: &PgPool, source: &str) {
        sqlx::query("DELETE FROM self_certifying_shard_keys WHERE source_url = $1")
            .bind(source)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn shard_cap_key_pinning_and_source_cap_hold_in_the_database() {
        let _guard = DB_LOCK.lock().await;
        let pool = live_pool().await;
        let source = format!("http://cap-{}", uuid::Uuid::new_v4());
        let bounds = MirrorBounds {
            max_shards: total(&pool).await + 2,
            max_shards_per_source: 5,
            ..MirrorBounds::default()
        };
        let keys: Vec<_> = (0..3).map(fresh).collect();
        let pin = |i: usize| {
            let (pool, source) = (pool.clone(), source.clone());
            let (k, id) = (keys[i].0.verifying_key(), keys[i].1.clone());
            async move { pin_key(&pool, &id, &k, &source, &bounds).await }
        };
        assert_eq!(pin(0).await, Ok(()));
        assert_eq!(pin(0).await, Ok(()));
        assert_eq!(pin(1).await, Ok(()));
        assert_eq!(pin(2).await, Err(Rejection::ShardCapReached));
        assert_eq!(
            can_admit(&pool, &keys[2].1, &source, &bounds).await,
            Err(Rejection::ShardCapReached)
        );
        assert_eq!(
            pin_key(
                &pool,
                &keys[0].1,
                &keys[1].0.verifying_key(),
                &source,
                &bounds
            )
            .await,
            Err(Rejection::KeyDoesNotMatchId)
        );
        assert_eq!(
            pinned_key(&pool, &keys[0].1).await,
            Some(keys[0].0.verifying_key())
        );
        cleanup(&pool, &source).await;
    }

    #[tokio::test]
    #[ignore]
    async fn per_source_cap_holds() {
        let _guard = DB_LOCK.lock().await;
        let pool = live_pool().await;
        let source = format!("http://src-{}", uuid::Uuid::new_v4());
        let bounds = MirrorBounds {
            max_shards: total(&pool).await + 10,
            max_shards_per_source: 1,
            ..MirrorBounds::default()
        };
        let (a, b) = (fresh(1), fresh(2));
        assert_eq!(
            pin_key(&pool, &a.1, &a.0.verifying_key(), &source, &bounds).await,
            Ok(())
        );
        assert_eq!(
            pin_key(&pool, &b.1, &b.0.verifying_key(), &source, &bounds).await,
            Err(Rejection::SourceCapReached)
        );
        assert_eq!(
            can_admit(&pool, &b.1, &source, &bounds).await,
            Err(Rejection::SourceCapReached)
        );
        cleanup(&pool, &source).await;
    }

    #[tokio::test]
    #[ignore]
    async fn idle_pin_is_evicted_but_an_active_pin_is_not() {
        let _guard = DB_LOCK.lock().await;
        let pool = live_pool().await;
        let source = format!("http://idle-{}", uuid::Uuid::new_v4());
        let bounds = MirrorBounds {
            max_shards: total(&pool).await + 2,
            max_shards_per_source: 10,
            idle_secs: 3600,
            ..MirrorBounds::default()
        };
        let (old, active, new) = (fresh(1), fresh(2), fresh(3));
        for k in [&old, &active] {
            pin_key(&pool, &k.1, &k.0.verifying_key(), &source, &bounds)
                .await
                .unwrap();
        }
        // Full with only active pins: a new shard is refused.
        assert_eq!(
            pin_key(&pool, &new.1, &new.0.verifying_key(), &source, &bounds).await,
            Err(Rejection::ShardCapReached)
        );
        sqlx::query("UPDATE self_certifying_shard_keys SET last_seen_at = now() - interval '2 hours' WHERE shard_id = $1")
            .bind(&old.1)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(can_admit(&pool, &new.1, &source, &bounds).await, Ok(()));
        assert_eq!(
            pin_key(&pool, &new.1, &new.0.verifying_key(), &source, &bounds).await,
            Ok(())
        );
        assert_eq!(pinned_key(&pool, &old.1).await, None);
        assert!(pinned_key(&pool, &active.1).await.is_some());
        cleanup(&pool, &source).await;
    }

    #[tokio::test]
    #[ignore]
    async fn concurrent_pins_never_exceed_the_cap() {
        let _guard = DB_LOCK.lock().await;
        let pool = live_pool().await;
        let source = format!("http://race-{}", uuid::Uuid::new_v4());
        let bounds = MirrorBounds {
            max_shards: total(&pool).await + 3,
            max_shards_per_source: 100,
            ..MirrorBounds::default()
        };
        let mut tasks = Vec::new();
        for i in 0..12u8 {
            let (pool, source) = (pool.clone(), source.clone());
            tasks.push(tokio::spawn(async move {
                let (k, id) = fresh(i);
                pin_key(&pool, &id, &k.verifying_key(), &source, &bounds).await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            if t.await.unwrap().is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, 3);
        cleanup(&pool, &source).await;
    }
}
