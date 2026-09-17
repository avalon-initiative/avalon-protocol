//! Cross-shard root computation, server side (issue #529, implementing
//! #527's decided sharded-settlement design). `avalon_chain::cross_shard`
//! has the pure aggregation math; this module is the network-facing
//! half — gathering `(shard_id, SignedTreeHead)` pairs from configured
//! shard peers (`GET /ledger/sth/latest`, the same read every mirror
//! already uses — no new transport), verifying each, and serving the
//! result at `GET /ledger/cross-shard-root`.
//!
//! **Shard discovery is still config-based (`AVALON_KNOWN_SHARDS`) — which
//! shards to even ask about isn't discovered automatically. Trust is not**:
//! #543's real mechanism, [`resolve_shard_verify_keys_from_db`], resolves
//! a shard's authorized key(s) directly from this node's own `issuer_keys`
//! table (`purpose = 'shard_settlement'`) — the same registration flow
//! attestation-issuance keys already use, not a second registry — and is
//! tried first. `AVALON_SHARD_VERIFY_KEYS` is a fallback for a shard whose
//! key hasn't resolved from the DB at all (e.g. this node has never seen
//! that integrator's registration), the same "interim, per-node config"
//! shape issue #531's `AVALON_MANAGED_HOSTING_VERIFY_KEY` established.
//! Live-verified across two genuinely separate machines
//! (`docs/architecture/settlement.md`'s "Cross-machine, real end to end"
//! section). A shard with no key resolved from either source, or whose
//! STH fails to fetch or verify, is treated exactly like a shard this
//! node has never heard an STH for — folded into `missing_shard_ids`,
//! never silently included unverified (see
//! `avalon_chain::cross_shard::compute_cross_shard_root_checked`'s own
//! doc comment).
//!
//! **Unset (the default): the one-shard degenerate case, no network
//! calls.** `AVALON_KNOWN_SHARDS` unset means this node's own local STH
//! (if it has one) is the entire cross-shard root, computed directly from
//! `state.chain` rather than an HTTP round trip to itself — exactly
//! #529's own "a network with one shard degenerates to a tree over a
//! single leaf" case, not a separate code path.

use std::collections::{BTreeSet, HashMap};

use avalon_chain::cross_shard::{compute_cross_shard_root_checked, CrossShardRoot, ShardTreeHead};
use avalon_chain::sth::{self, SignedTreeHead};
use axum::extract::State;
use axum::Json;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sqlx::Row;
use time::OffsetDateTime;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Clone)]
pub struct KnownShardsConfig {
    urls: HashMap<String, String>,
    verify_keys: HashMap<String, VerifyingKey>,
}

impl KnownShardsConfig {
    /// `AVALON_KNOWN_SHARDS` unset returns `None` — see module doc
    /// comment for the one-shard-degenerate default this leaves
    /// [`cross_shard_root_response`] to fall back to.
    pub fn from_env() -> Option<Self> {
        let urls_raw = std::env::var("AVALON_KNOWN_SHARDS")
            .ok()
            .filter(|s| !s.is_empty())?;
        let mut urls = HashMap::new();
        for entry in urls_raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let Some((shard_id, url)) = entry.split_once('=') else {
                tracing::error!(entry, "AVALON_KNOWN_SHARDS entry missing '=' — ignoring");
                continue;
            };
            urls.insert(
                shard_id.trim().to_string(),
                url.trim().trim_end_matches('/').to_string(),
            );
        }
        if urls.is_empty() {
            return None;
        }

        let mut verify_keys = HashMap::new();
        if let Ok(keys_raw) = std::env::var("AVALON_SHARD_VERIFY_KEYS") {
            for entry in keys_raw.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let Some((shard_id, hex_key)) = entry.split_once('=') else {
                    tracing::error!(
                        entry,
                        "AVALON_SHARD_VERIFY_KEYS entry missing '=' — ignoring"
                    );
                    continue;
                };
                match parse_verify_key(hex_key.trim()) {
                    Some(key) => {
                        verify_keys.insert(shard_id.trim().to_string(), key);
                    }
                    None => tracing::error!(
                        shard_id,
                        "AVALON_SHARD_VERIFY_KEYS entry is not a valid hex Ed25519 key — ignoring"
                    ),
                }
            }
        }

        Some(Self { urls, verify_keys })
    }

    pub fn known_shard_ids(&self) -> BTreeSet<String> {
        self.urls.keys().cloned().collect()
    }
}

fn parse_verify_key(hex_value: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(hex_value).ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// Same shape `mirror_watcher.rs`'s own `SignedTreeHeadDto` already uses
/// for parsing a peer's `GET /ledger/sth/latest` response — duplicated
/// rather than shared, matching this codebase's established "small
/// per-module DTO, not a shared internal type" convention (that struct is
/// private to its own module).
#[derive(Deserialize)]
struct FetchedSth {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl From<FetchedSth> for SignedTreeHead {
    fn from(dto: FetchedSth) -> Self {
        SignedTreeHead {
            tree_size: dto.tree_size,
            root_hash: dto.root_hash,
            network_id: dto.network_id,
            signing_key_id: dto.signing_key_id,
            signature: dto.signature,
            created_at: dto.created_at,
        }
    }
}

/// Issue #543: resolves a shard's currently-authorized
/// `shard_settlement`-purpose issuer keys directly from this node's own
/// `issuer_keys` table — the real per-shard trust-anchor mechanism,
/// composing with #529's aggregation rather than relying solely on the
/// interim `AVALON_SHARD_VERIFY_KEYS` static config. `shard_id` must be
/// `"{namespace}:{owner}"` (#532's own derivation, e.g.
/// `"game:ashen-realms"`) to resolve at all — `"core"` (no owning
/// integrator) and any other unparseable id return no keys, same as an
/// integrator that has never registered a `shard_settlement` key. Only
/// currently-unrevoked keys are returned — rotation/compromise reuses
/// `issuer.key_revoked` unchanged, per this mechanism's own design.
async fn resolve_shard_verify_keys_from_db(state: &AppState, shard_id: &str) -> Vec<VerifyingKey> {
    let Some((namespace, owner)) = shard_id.split_once(':') else {
        return Vec::new();
    };
    if !matches!(namespace, "game" | "app" | "service") {
        return Vec::new();
    }

    let rows = sqlx::query(
        "SELECT ik.public_key FROM issuer_keys ik \
         JOIN integrators i ON i.id = ik.integrator_id \
         WHERE i.slug = $1 AND i.category = $2 \
           AND ik.purpose = 'shard_settlement' AND ik.revoked_at IS NULL",
    )
    .bind(owner)
    .bind(namespace)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .filter_map(|row| {
            let bytes: Vec<u8> = row.try_get::<Vec<u8>, _>("public_key").ok()?;
            let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
            VerifyingKey::from_bytes(&array).ok()
        })
        .collect()
}

/// Fetches, verifies, and aggregates every configured known shard's
/// current STH. Returns the [`CrossShardRoot`] plus the exact
/// `(shard_id, SignedTreeHead)` pairs it was computed from — #529's own
/// "publishes the root plus the full list it used, so anyone can
/// independently verify by recomputing" requirement.
///
/// **Verification order (#543 composing with #529)**: a shard's STH is
/// checked against every currently-authorized `shard_settlement` key this
/// node can resolve from its own `issuer_keys` table first — the real
/// mechanism — and, if none resolve (e.g. `"core"`, or an integrator that
/// hasn't registered one yet), falls back to the interim
/// `AVALON_SHARD_VERIFY_KEYS` static config. Either source succeeding is
/// sufficient; neither resolving at all is exactly "no verify key
/// configured," folded into `missing_shard_ids` like any other
/// unverifiable shard.
pub async fn fetch_and_compute(
    state: &AppState,
    config: &KnownShardsConfig,
) -> (CrossShardRoot, Vec<ShardTreeHead>) {
    let client = reqwest::Client::new();
    let mut shards = Vec::new();

    for (shard_id, url) in &config.urls {
        let db_keys = resolve_shard_verify_keys_from_db(state, shard_id).await;
        let static_key = config.verify_keys.get(shard_id);
        if db_keys.is_empty() && static_key.is_none() {
            tracing::warn!(
                shard_id,
                "cross-shard root: no verify key resolved (neither issuer-key registration \
                 nor static config) for this shard, treating as missing"
            );
            continue;
        }

        let fetched: Result<FetchedSth, String> = async {
            let response = client
                .get(format!("{url}/ledger/sth/latest"))
                // Issue #573: explicit, not implicit — a peer answering
                // for more than one shard (mirroring one, authoring
                // another) needs to be told which one this request is
                // about; omitting it would silently get whichever shard
                // that peer treats as its own default (`"core"`), not
                // necessarily the one this aggregation actually wants.
                .query(&[("shard_id", shard_id.as_str())])
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?;
            response
                .json::<FetchedSth>()
                .await
                .map_err(|e| e.to_string())
        }
        .await;

        let sth: SignedTreeHead = match fetched {
            Ok(dto) => dto.into(),
            Err(err) => {
                tracing::warn!(
                    shard_id,
                    error = %err,
                    "cross-shard root: STH fetch failed, treating as missing"
                );
                continue;
            }
        };

        let verified = db_keys
            .iter()
            .chain(static_key)
            .any(|key| sth::verify_tree_head(key, &sth));
        if !verified {
            tracing::warn!(
                shard_id,
                "cross-shard root: STH signature verification failed against every resolved \
                 key, treating as missing"
            );
            continue;
        }

        shards.push(ShardTreeHead {
            shard_id: shard_id.clone(),
            sth,
        });
    }

    let known = config.known_shard_ids();
    let root = compute_cross_shard_root_checked(&known, shards.clone(), OffsetDateTime::now_utc());
    (root, shards)
}

/// Builds the response `GET /ledger/cross-shard-root` serves — either the
/// full multi-shard fetch-and-aggregate path (`AVALON_KNOWN_SHARDS` set),
/// or the one-shard degenerate default (unset): this node's own local
/// STH alone, computed directly, no network calls.
pub async fn compute_for_this_node(
    state: &AppState,
    config: Option<&KnownShardsConfig>,
) -> Result<(CrossShardRoot, Vec<ShardTreeHead>), avalon_chain::SettlementError> {
    if let Some(config) = config {
        return Ok(fetch_and_compute(state, config).await);
    }

    let shards = match state.chain.latest_signed_tree_head().await? {
        Some(sth) => vec![ShardTreeHead {
            shard_id: "core".to_string(),
            sth,
        }],
        None => Vec::new(),
    };
    let known: BTreeSet<String> = shards.iter().map(|s| s.shard_id.clone()).collect();
    let root = compute_cross_shard_root_checked(&known, shards.clone(), OffsetDateTime::now_utc());
    Ok((root, shards))
}

#[derive(serde::Serialize)]
pub struct ShardSthEntry {
    pub shard_id: String,
    pub tree_size: i64,
    pub root_hash: String,
    pub signing_key_id: String,
    pub signature: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(serde::Serialize)]
pub struct CrossShardRootResponse {
    pub root_hash: String,
    pub shard_count: usize,
    pub partial: bool,
    pub missing_shard_ids: Vec<String>,
    /// The exact `(shard_id, sth)` pairs `root_hash` was computed from —
    /// #529's own "publishes the root plus the full list it used, so
    /// anyone can independently verify by recomputing" requirement. Never
    /// itself signed — see `avalon_chain::cross_shard`'s module doc
    /// comment for why signing this would reintroduce a designated-
    /// aggregator chokepoint.
    pub shards: Vec<ShardSthEntry>,
}

/// `GET /ledger/cross-shard-root` — public, unauthenticated, same posture
/// every other transparency-log read in `crate::settlement` already has:
/// a cross-shard root is exactly as independently verifiable as a
/// per-shard STH, and gating it behind a credential would defeat that.
pub async fn cross_shard_root(
    State(state): State<AppState>,
) -> Result<Json<CrossShardRootResponse>, AppError> {
    let (root, shards) = compute_for_this_node(&state, state.known_shards.as_ref()).await?;
    Ok(Json(CrossShardRootResponse {
        root_hash: root.root_hash,
        shard_count: root.shard_count,
        partial: root.partial,
        missing_shard_ids: root.missing_shard_ids,
        shards: shards
            .into_iter()
            .map(|s| ShardSthEntry {
                shard_id: s.shard_id,
                tree_size: s.sth.tree_size,
                root_hash: s.sth.root_hash,
                signing_key_id: s.sth.signing_key_id,
                signature: s.sth.signature,
                created_at: s.sth.created_at,
            })
            .collect(),
    }))
}
