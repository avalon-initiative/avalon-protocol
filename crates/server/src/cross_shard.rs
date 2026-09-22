//! Cross-shard root computation, server side (issue #529, implementing
//! #527's decided sharded-settlement design). `avalon_chain::cross_shard`
//! has the pure aggregation math; this module is the network-facing
//! half — gathering `(shard_id, SignedTreeHead)` pairs from configured
//! shard peers (`GET /ledger/sth/latest`, the same read every mirror
//! already uses — no new transport), verifying each, and serving the
//! result at `GET /ledger/cross-shard-root`.
//!
//! **Shard discovery (issue #599): `AVALON_KNOWN_SHARDS` is now additive,
//! not the only source.** Before #599, which shards to even ask about was
//! purely config-based — this module's own doc comment used to say so
//! plainly. Now [`combined_shard_urls`] unions `AVALON_KNOWN_SHARDS`'s
//! static map with whatever `crate::nodes::ShardRegistry` has learned via
//! peer-announce gossip (#599's Layer 2), so a node with zero
//! `AVALON_KNOWN_SHARDS` configured at all can still aggregate a real,
//! multi-shard cross-shard root purely from what it's discovered. Trust is
//! unchanged either way: #543's real mechanism,
//! [`resolve_shard_verify_keys_from_db`], resolves a shard's authorized
//! key(s) directly from this node's own `issuer_keys` table (`purpose =
//! 'shard_settlement'`) — the same registration flow attestation-issuance
//! keys already use, not a second registry — and is tried first, for a
//! statically-configured shard and a discovered one alike.
//! `AVALON_SHARD_VERIFY_KEYS` is a fallback for a statically-configured
//! shard whose key hasn't resolved from the DB at all (e.g. this node has
//! never seen that integrator's registration) — a purely
//! gossip-discovered shard has no static verify key at all, so it relies
//! on the DB resolution alone, which is the point: discovering it is
//! enough, an operator never has to also hand-configure its key. A shard
//! with no key resolved from either source, or whose STH fails to fetch
//! or verify, is treated exactly like a shard this node has never heard
//! an STH for — folded into `missing_shard_ids`, never silently included
//! unverified (see
//! `avalon_chain::cross_shard::compute_cross_shard_root_checked`'s own
//! doc comment).
//!
//! **Neither configured nor discovered: the one-shard degenerate case, no
//! network calls.** This node's own local STH (if it has one) is the
//! entire cross-shard root, computed directly from `state.chain` rather
//! than an HTTP round trip to itself — exactly #529's own "a network with
//! one shard degenerates to a tree over a single leaf" case, not a
//! separate code path.

use std::collections::{BTreeSet, HashMap};

use avalon_chain::cross_shard::{compute_cross_shard_root_checked, CrossShardRoot, ShardTreeHead};
use avalon_protocol::sth::{self, SignedTreeHead};
use axum::extract::State;
use axum::Json;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::error::AppError;
use crate::nodes::ShardRegistry;
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

    /// Issue #599: read access for [`combined_shard_urls`]/mirror_watcher's
    /// own auto-discovery pass, which need to know what's *already*
    /// explicitly configured so they don't treat an explicit entry as a
    /// newly-discovered one.
    pub fn urls(&self) -> &HashMap<String, String> {
        &self.urls
    }
}

/// Unions `config`'s static `AVALON_KNOWN_SHARDS` map with whatever
/// `registry` has learned via peer-announce gossip (issue #599) — the
/// static config wins for a `shard_id` present in both, since an operator
/// who explicitly configured a URL presumably wants that one used, not
/// whatever a peer happens to be gossiping. A `shard_id` known only to the
/// registry uses its most-recently-seen URL ([`ShardRegistry::best_url`]);
/// still independently verified below exactly like any other shard — this
/// only decides which URL is worth trying.
pub fn combined_shard_urls(
    config: Option<&KnownShardsConfig>,
    registry: &ShardRegistry,
) -> HashMap<String, String> {
    let mut urls = config.map(|c| c.urls.clone()).unwrap_or_default();
    for shard_id in registry.known_shard_ids() {
        if urls.contains_key(&shard_id) {
            continue;
        }
        if let Some(url) = registry.best_url(&shard_id) {
            urls.insert(shard_id, url);
        }
    }
    urls
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
pub(crate) async fn resolve_shard_verify_keys_from_db(
    pool: &PgPool,
    shard_id: &str,
) -> Vec<VerifyingKey> {
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
    .fetch_all(pool)
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

/// Fetches, verifies, and aggregates every known shard's current STH —
/// "known" meaning [`combined_shard_urls`]'s union of static config and
/// gossip-discovered shards (issue #599). Returns the [`CrossShardRoot`]
/// plus the exact `(shard_id, SignedTreeHead)` pairs it was computed from
/// — #529's own "publishes the root plus the full list it used, so anyone
/// can independently verify by recomputing" requirement.
///
/// **Verification order (#543 composing with #529)**: a shard's STH is
/// checked against every currently-authorized `shard_settlement` key this
/// node can resolve from its own `issuer_keys` table first — the real
/// mechanism, and the *only* one available for a purely gossip-discovered
/// shard — and, if none resolve (e.g. `"core"`, or an integrator that
/// hasn't registered one yet), falls back to `static_verify_keys`
/// (`AVALON_SHARD_VERIFY_KEYS`, only ever populated for a
/// statically-configured shard). Either source succeeding is sufficient;
/// neither resolving at all is exactly "no verify key configured," folded
/// into `missing_shard_ids` like any other unverifiable shard.
pub async fn fetch_and_compute(
    pool: &PgPool,
    urls: &HashMap<String, String>,
    static_verify_keys: &HashMap<String, VerifyingKey>,
) -> (CrossShardRoot, Vec<ShardTreeHead>) {
    let client = reqwest::Client::new();
    let mut shards = Vec::new();

    for (shard_id, url) in urls {
        let db_keys = resolve_shard_verify_keys_from_db(pool, shard_id).await;
        let static_key = static_verify_keys.get(shard_id);
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

    let known: BTreeSet<String> = urls.keys().cloned().collect();
    let root = compute_cross_shard_root_checked(&known, shards.clone(), OffsetDateTime::now_utc());
    (root, shards)
}

/// Builds the response `GET /ledger/cross-shard-root` serves. This node's
/// own locally-authored shard (`state.own_shard_id`, `"core"` by default)
/// is always included when it has any local history at all, resolved
/// directly from `state.chain` rather than an HTTP round trip to itself —
/// never gated behind whether any *other* shard happens to be known too.
/// Before issue #599 this was an either/or branch (either the full
/// multi-shard fetch-and-aggregate path, or a "no other shards known"
/// degenerate default of just the local shard) — that silently dropped a
/// node's own shard out of the aggregation the moment it also discovered
/// (via gossip) or was configured with any other shard, which is exactly
/// the shape a gossip-participating shard-authority node now commonly has.
/// Any URL entry that happens to name this node's own shard (a
/// self-referential gossip claim or static config entry) is removed from
/// the fetch set — the local read is always used instead of fetching from
/// itself.
pub async fn compute_for_this_node(
    state: &AppState,
    config: Option<&KnownShardsConfig>,
) -> Result<(CrossShardRoot, Vec<ShardTreeHead>), avalon_chain::SettlementError> {
    let mut urls = combined_shard_urls(config, &state.shard_registry);
    urls.remove(&state.own_shard_id);

    let (mut shards, mut known): (Vec<ShardTreeHead>, BTreeSet<String>) = if urls.is_empty() {
        (Vec::new(), BTreeSet::new())
    } else {
        let static_verify_keys = config.map(|c| c.verify_keys.clone()).unwrap_or_default();
        let (_urls_only_root, ext_shards) =
            fetch_and_compute(&state.pool, &urls, &static_verify_keys).await;
        (ext_shards, urls.keys().cloned().collect())
    };

    if let Some(sth) = state.chain.latest_signed_tree_head().await? {
        known.insert(state.own_shard_id.clone());
        shards.push(ShardTreeHead {
            shard_id: state.own_shard_id.clone(),
            sth,
        });
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with(shard_id: &str, url: &str) -> ShardRegistry {
        let registry = ShardRegistry::new();
        registry.record_own(shard_id, url, OffsetDateTime::now_utc());
        registry
    }

    #[test]
    fn discovered_shards_are_included_when_no_static_config_exists() {
        let registry = registry_with("game:ashen-realms", "http://discovered.invalid");
        let urls = combined_shard_urls(None, &registry);
        assert_eq!(
            urls.get("game:ashen-realms"),
            Some(&"http://discovered.invalid".to_string())
        );
    }

    #[test]
    fn static_config_wins_over_a_discovered_url_for_the_same_shard() {
        let registry = registry_with("game:ashen-realms", "http://discovered.invalid");
        let config = KnownShardsConfig {
            urls: HashMap::from([(
                "game:ashen-realms".to_string(),
                "http://configured.invalid".to_string(),
            )]),
            verify_keys: HashMap::new(),
        };
        let urls = combined_shard_urls(Some(&config), &registry);
        assert_eq!(
            urls.get("game:ashen-realms"),
            Some(&"http://configured.invalid".to_string()),
            "an explicitly configured URL must never be silently overridden by a gossiped one"
        );
    }

    #[test]
    fn config_and_discovered_shards_not_overlapping_are_both_present() {
        let registry = registry_with("game:other-title", "http://discovered.invalid");
        let config = KnownShardsConfig {
            urls: HashMap::from([(
                "game:ashen-realms".to_string(),
                "http://configured.invalid".to_string(),
            )]),
            verify_keys: HashMap::new(),
        };
        let urls = combined_shard_urls(Some(&config), &registry);
        assert_eq!(urls.len(), 2);
        assert!(urls.contains_key("game:ashen-realms"));
        assert!(urls.contains_key("game:other-title"));
    }

    #[test]
    fn no_config_and_empty_registry_yields_no_urls() {
        let registry = ShardRegistry::new();
        let urls = combined_shard_urls(None, &registry);
        assert!(urls.is_empty());
    }
}
