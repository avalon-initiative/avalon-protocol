//! The mirror-watcher — verifies and stores STHs/entries
//! polled from configured peers, run as a background task inside
//! `avalon-server`. See `avalon-docs/architecture/nodes/README.md`'s "Today in the
//! repo" section for the multi-peer polling/backfill/equivocation-
//! detection design and why it lives in-process rather than as a CLI
//! daemon.
//!
//! **Two deliberate tiers.** Polling every configured peer on
//! [`MirrorWatcherConfig::poll_interval`] is the permissionless baseline —
//! mirroring this way needs zero registration and never will, since a
//! public transparency log must never gate reading on registering with
//! anyone. On top of that, this worker also registers this node's own
//! interest (via `crate::interest::InterestScope::for_network`, the exact
//! same DHT-backed registration built for guild-channel/conversation
//! routing) in every `network_id` it successfully verifies an STH for —
//! never in one it merely hopes to mirror, so a hostile or unpinned
//! `network_id` a peer might claim is never registered either. A
//! committing authority (`crate::mirror_push::notify_peers`) resolves that
//! registration and pushes a lightweight "go check" notification straight
//! to this node, which wakes [`run_worker`]'s loop early via
//! `AppState::mirror_wake` — see `crate::mirror_push`'s own module doc for
//! the delivery mechanics and why a push is never trusted directly. Poll's
//! role for a peer that's actually receiving pushes shrinks to "bound
//! worst-case staleness if a push is ever missed or this node's DHT
//! identity is disabled" — which is why [`MirrorWatcherConfig::from_env`]'s
//! default interval is now meaningfully longer than it used to be; see
//! its own doc comment.
//!
//! **`AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` opts a node
//! into auto-mirroring every shard it discovers via peer-announce gossip
//! (`crate::nodes::ShardRegistry`), on top of whatever `AVALON_MIRROR_PEERS`
//! explicitly names.** [`discover_and_verify_shard_peers`] is the
//! deliberately separate verification path this needs: a
//! statically-configured `AVALON_MIRROR_PEERS` entry is verified against
//! this process's pinned `docs/trusted-networks.json` network trust
//! anchor (`verify_head_against_anchors`, below) — the right check for mirroring
//! another *whole network*, except a configured `node:<hash>` shard, which is verified by its own
//! key like a discovered one; a source may be http(s) or `p2p://` (#1147). A gossip-discovered shard is a different
//! shard *within this node's own network*, signed with an
//! integrator-registered `shard_settlement` key, which almost
//! never matches this network's root trust-anchor key — so a discovered
//! shard's STH is verified via `crate::cross_shard::resolve_shard_verify_keys_from_db`
//! instead, the exact same mechanism `crate::cross_shard`'s own
//! aggregation already uses. Only a shard whose STH verifies this way is
//! ever added to this tick's backfill targets; discovering a shard's
//! existence never implies trusting it. `AVALON_MIRROR_PEERS`/`AVALON_KNOWN_SHARDS` remain
//! valid, unchanged, narrower configuration — this is additive. Push
//! registration and discovered-shard auto-mirroring are independent
//! of each other and compose without special-casing: a discovered shard is
//! just another network-scoped mirror target as far as push/interest
//! registration is concerned.

//!
//! **What backfill verifies before storing an entry.** (1) The head is
//! signature-checked (or pinned for a self-certifying shard) and corroborated.
//! (2) The entry's inclusion proof verifies against that head's root at the
//! entry's leaf index. (3) The entry's hash is recomputed from its content
//! (event id, kind, issuer, subject, payload, timestamp, version, `prev_hash`
//! and network id) and must equal both the claimed `entry_hash` and the proof
//! leaf, so served content is bound to the proven hash. (4) Its `prev_hash`
//! must equal the previous verified entry's hash (genesis for the first), so
//! the hash chain links. (5) Its `seq` must exceed the last mirrored `seq`
//! (gaps are legitimate; `seq` is not covered by the hash, so this is only an
//! ordering and plausibility check) and may jump at most 2^32 past it; a larger
//! jump is refused loudly with the source, both seqs and the bound logged. An entry and its proof always come from
//! the same candidate source. A candidate whose entry fails any check, or
//! serves nothing, is excluded for the rest of the tick and the same page is
//! retried from the next candidate; nothing from a refused entry is stored,
//! and the tick errors only when every candidate failed.
//! `avalon_chain::mirror::insert_mirrored_entry` repeats (3) at the storage
//! boundary, and a row that already exists at the entry's `seq` is an error.
//! **Pruned payloads are refused** (`payload_pruned`): a hash cannot be
//! recomputed without the payload, so a pruned source is skipped and the shard
//! mirrors only if another candidate keeps full history. A served JSON null
//! payload that is not flagged pruned is a real payload and is hashed as null.
//! A pruned or unverified entry is never stored or projected.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use avalon_chain::mirror::{self, ObservedSth, SELF_SIGNED_SOURCE};
use avalon_chain::{merkle, PostgresSettlementProvider};
use avalon_indexer::postgres::PostgresIndexer;
use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::event_payloads::IdentityCreatedPayload;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::{GlobalId, IdentityId};
use avalon_protocol::sth::SignedTreeHead;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sqlx::Acquire;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cosign_gather;
use crate::cosign_verify::{self, WitnessCosignatureDto};
use crate::known_list::KnownListHandle;
use crate::nodes::HeadGossipTracker;
use crate::witness_cosign::{self, WitnessCosignConfig};

/// How many entries to request per bulk-entries page while backfilling.
const BACKFILL_PAGE_SIZE: i64 = 200;

/// Most `p2p://` entries `AVALON_MIRROR_PEERS` may name; further ones are dropped.
pub(crate) const MAX_P2P_MIRROR_SOURCES: usize = 16;

/// Issue #573: `AVALON_MIRROR_PEERS` entries are a bare URL (the `"core"` shard) or
/// `shard_id=url`. A URL may be a canonical `p2p://<peer id>` (#1146); other `p2p` spellings,
/// malformed ids and p2p entries past [`MAX_P2P_MIRROR_SOURCES`] are dropped, warning if `warn`.
pub(crate) fn parse_mirror_peers(raw: &str, warn: bool) -> Vec<(String, String)> {
    let drop_entry = |entry: &str, why: &str| {
        if warn {
            tracing::warn!(entry, "mirror peers: dropping entry: {why}");
        }
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut p2p_sources = 0;
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // A p2p entry is a whole bare URL, so a stray `=` in it is not a shard separator.
        let (shard_id, url) = match entry.split_once('=') {
            Some((shard_id, url)) if !is_p2p_scheme(entry) => {
                (shard_id.trim().to_string(), url.trim())
            }
            _ => ("core".to_string(), entry),
        };
        let url = if is_p2p_scheme(url) {
            // The canonical string is also the DB key for this source's rows.
            let canonical = crate::node_http::is_p2p_url(url)
                .then(|| crate::node_http::parse_p2p_base(url))
                .flatten();
            let Some(peer) = canonical else {
                drop_entry(entry, "malformed or non-canonical p2p:// source");
                continue;
            };
            crate::node_http::p2p_base_url(&peer)
        } else {
            url.trim_end_matches('/').to_string()
        };
        if url.is_empty() || out.iter().any(|(s, u)| *s == shard_id && *u == url) {
            continue;
        }
        if crate::node_http::is_p2p_url(&url) {
            if p2p_sources >= MAX_P2P_MIRROR_SOURCES {
                drop_entry(entry, "too many p2p:// sources");
                continue;
            }
            p2p_sources += 1;
        }
        out.push((shard_id, url));
    }
    out
}

/// Whether `url` uses the `p2p` scheme in any case or spelling (`P2P://x`, `p2p:x`).
fn is_p2p_scheme(url: &str) -> bool {
    url.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("p2p:"))
}

static DEFAULT_CORE_MIRROR_PEERS: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Core mirror source for a node that authors no `core` and sets no
/// `AVALON_MIRROR_PEERS`: the network's seed nodes. Empty when there are none.
pub fn resolve_default_core_mirror_peers(
    own_shard_id: &str,
    mirror_peers_env: Option<&str>,
    network_id: &str,
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
) -> String {
    if own_shard_id == avalon_protocol::shard::CORE_SHARD_ID
        || mirror_peers_env.is_some_and(|v| !v.trim().is_empty())
    {
        return String::new();
    }
    anchors
        .iter()
        .find(|a| a.network_id == network_id)
        .map(|a| a.seed_nodes.join(","))
        .unwrap_or_default()
}

/// Records the default resolved at startup; call once, before any config is read.
pub fn set_default_core_mirror_peers(value: String) {
    let _ = DEFAULT_CORE_MIRROR_PEERS.set(value);
}

/// `AVALON_MIRROR_PEERS` when set, otherwise the startup default.
pub fn effective_mirror_peers() -> String {
    match std::env::var("AVALON_MIRROR_PEERS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => DEFAULT_CORE_MIRROR_PEERS.get().cloned().unwrap_or_default(),
    }
}

pub struct MirrorWatcherConfig {
    /// `(shard_id, base_url)` pairs to watch, e.g.
    /// `("core", "http://localhost:8081")` — no trailing slash on the URL
    /// (stripped in [`Self::from_env`] if present). More than one is a
    /// normal, supported configuration — see module docs. Issue #604:
    /// `shard_id` is carried alongside each URL (not discarded) precisely
    /// because polling *does* need to know which shard it's asking a peer
    /// about — a peer serving more than one shard answers a bare
    /// `/ledger/sth/latest` with whichever it treats as its own default,
    /// not necessarily the one this entry means (#573's documented
    /// footgun).
    pub peers: Vec<(String, String)>,
    pub poll_interval: Duration,
    /// Every `shard_id` explicitly named in `AVALON_MIRROR_PEERS`
    /// (`"core"` for a bare, un-prefixed entry) — issue #599's
    /// auto-discovery pass skips these, since an operator who explicitly
    /// configured a shard already gets it mirrored via `peers` above and
    /// verified via the network-trust-anchor path; auto-discovery only
    /// ever adds a shard this config doesn't already name.
    pub known_shard_ids: BTreeSet<String>,
    /// `AVALON_MIRROR_ALL_DISCOVERED_SHARDS` — see this
    /// module's own doc comment. Opt-in, independent of whether `peers`
    /// above is empty: a node can auto-mirror discovered shards with zero
    /// explicit `AVALON_MIRROR_PEERS` configuration at all.
    pub auto_mirror_discovered: bool,
}

impl MirrorWatcherConfig {
    /// `Some` when either `AVALON_MIRROR_PEERS` names at least one peer,
    /// or `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` — either
    /// alone is enough for this worker to have something to do; `None`
    /// (neither set) is the signal `main.rs` uses to skip spawning the
    /// watcher entirely, the same "only spawn if configured" pattern
    /// `retention::RetentionConfig::should_prune` already uses.
    ///
    /// Poll interval defaults to 120s (raised from 30s),
    /// overridable via `AVALON_MIRROR_POLL_INTERVAL_SECS` — an STH is a
    /// few hundred bytes of JSON, so bandwidth was never the constraint;
    /// 30s was originally chosen as a sensible cadence for a purely
    /// poll-driven design. Now that a registered peer additionally gets a
    /// low-latency push the moment a new STH actually exists (see this
    /// module's own doc comment), polling's remaining job for that peer is
    /// only to bound worst-case staleness if a push is ever missed — which
    /// doesn't need a 30s cadence. 120s keeps that bound comfortably tight
    /// (a missed push costs at most two minutes of extra staleness, not
    /// thirty seconds) while cutting this worker's at-idle HTTP overhead
    /// by 4x for every deployment, registered or not — see
    /// `docs/projects/backend-server/architecture/nodes.md`'s mirror-sync section for the same
    /// reasoning written up for operators.
    pub fn from_env() -> Option<Self> {
        let peers = parse_mirror_peers(&effective_mirror_peers(), true);
        let known_shard_ids: BTreeSet<String> = peers
            .iter()
            .map(|(shard_id, _url)| shard_id.clone())
            .collect();

        let auto_mirror_discovered = std::env::var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);

        if peers.is_empty() && !auto_mirror_discovered {
            return None;
        }

        let poll_interval = std::env::var("AVALON_MIRROR_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS));
        Some(Self {
            peers,
            poll_interval,
            known_shard_ids,
            auto_mirror_discovered,
        })
    }
}

/// See [`MirrorWatcherConfig::from_env`]'s own doc comment for why this
/// changed from 30 to 120.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;

/// Verifies (never trusts by discovery alone) every
/// gossip-discovered shard this node hasn't already explicitly
/// configured, when `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`. See this
/// module's own doc comment for why this uses a different key-resolution
/// path (`crate::cross_shard::resolve_shard_verify_keys_from_db`) than
/// [`verify_head_against_anchors`] below — the acceptance rule itself (majority
/// cosignature against `known_list`, degenerating to plain
/// author-signature verification at 0 or 1) is the same.
/// Returns `(shard_id, url, CosignedTreeHead, author_verify_key)` for every
/// discovered shard whose STH verified — a shard that's unreachable, has no
/// registered key yet, or fails verification is simply left out this tick
/// (retried again next tick, never trusted on spec alone). The matched
/// author key is returned alongside the head for the same reason
/// [`verify_head_against_anchors`] returns one: a caller cross-checking two
/// accepted heads for equivocation needs the exact key both verified
/// against.
#[allow(clippy::too_many_arguments)]
async fn discover_and_verify_shard_peers(
    client: &crate::node_http::NodeClient,
    pool: &PgPool,
    network_id: &str,
    shard_registry: &crate::nodes::ShardRegistry,
    own_base_url: Option<&str>,
    already_configured: &BTreeSet<String>,
    bounds: &crate::self_certifying_keys::MirrorBounds,
    admitted_new: &BTreeSet<String>,
) -> Vec<(String, String, CosignedTreeHead, VerifyingKey)> {
    let mut verified = Vec::new();
    // Fetch attempts for unpinned self-certifying shards this tick, counted before the fetch.
    let mut attempts = 0usize;
    let mut shard_ids: Vec<String> = shard_registry.known_shard_ids().into_iter().collect();
    // Rotate the start so a few hostile shards cannot hold the attempts every tick.
    static TICK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tick = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !shard_ids.is_empty() {
        let by = tick % shard_ids.len();
        shard_ids.rotate_left(by);
    }
    for shard_id in shard_ids {
        if already_configured.contains(&shard_id) {
            continue;
        }
        let Some(url) = shard_registry.best_url(&shard_id) else {
            continue;
        };
        if Some(url.as_str()) == own_base_url {
            continue;
        }

        let self_certifying = avalon_protocol::shard_identity::is_self_certifying(&shard_id);
        let db_keys = if self_certifying {
            Vec::new()
        } else {
            crate::cross_shard::resolve_shard_verify_keys_from_db(pool, network_id, &shard_id).await
        };
        if db_keys.is_empty() && !self_certifying {
            tracing::info!(
                shard_id,
                url = %url,
                "mirror-watcher: discovered shard has no registered shard_settlement key \
                 resolved yet — not auto-mirroring until one is",
            );
            continue;
        }

        if self_certifying {
            match fetch_and_verify_self_certifying_sth(
                client,
                pool,
                network_id,
                &shard_id,
                &url,
                bounds,
                admitted_new,
                &mut attempts,
            )
            .await
            {
                Ok(Some((head, key))) => {
                    log_auto_mirror(&shard_id, &url);
                    verified.push((shard_id, url, head, key));
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    shard_id,
                    url = %url,
                    error = %err,
                    "mirror-watcher: not auto-mirroring a self-certifying shard",
                ),
            }
            continue;
        }

        match fetch_source_head(client, &url, &shard_id, SELF_CERTIFYING_FETCH_DEADLINE).await {
            Ok((dto, _)) => {
                let head: CosignedTreeHead = dto.into();
                let now = OffsetDateTime::now_utc();
                match cosign_verify::verify_cosigned_against_any_key(
                    db_keys.iter().copied(),
                    &head,
                    &[],
                    now,
                ) {
                    Some(matched_key) => {
                        log_auto_mirror(&shard_id, &url);
                        verified.push((shard_id, url, head, matched_key));
                    }
                    None => {
                        tracing::warn!(
                            shard_id,
                            url = %url,
                            "mirror-watcher: discovered shard's STH failed verification against \
                             every resolved key (or lacked majority cosignature) — not \
                             auto-mirroring",
                        );
                    }
                }
            }
            Err(err) => tracing::warn!(
                shard_id,
                url = %url,
                error = %err,
                "mirror-watcher: failed to fetch STH for a discovered shard",
            ),
        }
    }
    verified
}

fn log_auto_mirror(shard_id: &str, url: &str) {
    tracing::info!(
        event = "auto_mirror_discovered_shard",
        shard_id,
        url = %url,
        "auto-mirroring a newly discovered shard whose STH verified against a resolved shard key",
    );
}

/// Fetches a self-certifying shard's head from `source` (http(s) or `p2p://`) and verifies it
/// with [`verify_fetched_self_certifying_head`]. An unpinned shard costs one of
/// `bounds.max_new_per_tick` fetch `attempts` per tick, counted before the fetch whatever the
/// outcome, so hostile or dead sources cannot cause unbounded work.
#[allow(clippy::too_many_arguments)]
async fn fetch_and_verify_self_certifying_sth(
    client: &crate::node_http::NodeClient,
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source: &str,
    bounds: &crate::self_certifying_keys::MirrorBounds,
    admitted_new: &BTreeSet<String>,
    attempts: &mut usize,
) -> Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError> {
    if budget_spent(pool, shard_id, bounds, admitted_new).await {
        log_budget_skip(shard_id, source);
        return Ok(None);
    }
    if crate::self_certifying_keys::pinned_key(pool, shard_id)
        .await
        .is_none()
    {
        if *attempts >= bounds.max_new_per_tick {
            log_budget_skip(shard_id, source);
            return Ok(None);
        }
        *attempts += 1;
    }
    let fetched =
        fetch_source_head(client, source, shard_id, SELF_CERTIFYING_FETCH_DEADLINE).await?;
    verify_fetched_self_certifying_head(
        pool,
        network_id,
        shard_id,
        source,
        fetched,
        bounds,
        admitted_new,
    )
    .await
}

/// Whether `shard_id` is unpinned and this tick already admitted `max_new_per_tick` other
/// new shards. The same shard from a second source is never counted twice.
async fn budget_spent(
    pool: &PgPool,
    shard_id: &str,
    bounds: &crate::self_certifying_keys::MirrorBounds,
    admitted_new: &BTreeSet<String>,
) -> bool {
    !admitted_new.contains(shard_id)
        && admitted_new.len() >= bounds.max_new_per_tick
        && crate::self_certifying_keys::pinned_key(pool, shard_id)
            .await
            .is_none()
}

/// Whether `shard_id` is already pinned, or is not self-certifying and so never admitted.
async fn is_admitted(pool: &PgPool, shard_id: &str) -> bool {
    !avalon_protocol::shard_identity::is_self_certifying(shard_id)
        || crate::self_certifying_keys::pinned_key(pool, shard_id)
            .await
            .is_some()
}

/// Charges the admit budget once `shard_id` is actually pinned after being unpinned. A head
/// that was refused or is held for majority leaves the shard unpinned and spends no slot.
async fn charge_admission(
    pool: &PgPool,
    shard_id: &str,
    was_admitted: bool,
    admitted_new: &mut BTreeSet<String>,
) {
    if !was_admitted && is_admitted(pool, shard_id).await {
        admitted_new.insert(shard_id.to_string());
    }
}

/// Verifies an already fetched head of a self-certifying shard: the shard's own key, never a
/// trust anchor, vouches for it. `Ok(None)` when an unpinned shard is skipped because
/// `admitted_new` already holds `bounds.max_new_per_tick` shards. Nothing is charged here:
/// the admit budget is charged by [`charge_admission`] only when the shard gets pinned.
async fn verify_fetched_self_certifying_head(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source: &str,
    fetched: (SignedTreeHeadDto, String),
    bounds: &crate::self_certifying_keys::MirrorBounds,
    admitted_new: &BTreeSet<String>,
) -> Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError> {
    let (dto, peer_protocol_version) = fetched;
    if budget_spent(pool, shard_id, bounds, admitted_new).await {
        log_budget_skip(shard_id, source);
        return Ok(None);
    }
    let pinned = crate::self_certifying_keys::pinned_key(pool, shard_id).await;
    check_peer_version(source, &peer_protocol_version)?;
    let key =
        verify_self_certifying_shard(pool, network_id, shard_id, source, pinned, &dto, bounds)
            .await
            .map_err(MirrorWatcherError::SelfCertifyingRejected)?;
    let head: CosignedTreeHead = dto.into();
    let now = OffsetDateTime::now_utc();
    let matched = cosign_verify::verify_cosigned_against_any_key([key], &head, &[], now)
        .ok_or(MirrorWatcherError::InvalidSignature)?;
    Ok(Some((head, matched)))
}

/// Per-key throttle shared by the configured-source log lines.
fn permit_log(key: &str) -> Option<u64> {
    static LOG: std::sync::OnceLock<crate::log_throttle::LogThrottle> = std::sync::OnceLock::new();
    LOG.get_or_init(|| crate::log_throttle::LogThrottle::new(Duration::from_secs(300)))
        .permit(key, std::time::Instant::now())
}

fn log_budget_skip(shard_id: &str, source: &str) {
    if let Some(held_back) = permit_log(&format!("budget:{shard_id}:{source}")) {
        tracing::warn!(
            held_back,
            shard_id,
            source,
            "mirror-watcher: not examining a new self-certifying shard this tick, the per-tick \
             limit of new shards is spent (AVALON_MIRROR_SELF_CERTIFYING_NEW_PER_TICK)"
        );
    }
}

/// Verifies a self-certifying shard's head with only the key it presents (or the
/// key already pinned): the head must be for this node's network, the key must
/// hash to the shard id and sign the head, the shard must fit `bounds`, and a shard
/// not yet pinned must be admissible. Stores nothing: the key is pinned only once
/// the head is accepted.
async fn verify_self_certifying_shard(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source_url: &str,
    pinned: Option<VerifyingKey>,
    dto: &SignedTreeHeadDto,
    bounds: &crate::self_certifying_keys::MirrorBounds,
) -> Result<VerifyingKey, crate::self_certifying_keys::Rejection> {
    use crate::self_certifying_keys as keys;
    if dto.network_id != network_id {
        return Err(keys::Rejection::WrongNetwork);
    }
    let key = keys::select_key(shard_id, pinned, dto.signing_public_key.as_deref())?;
    let sth = SignedTreeHead {
        tree_size: dto.tree_size,
        root_hash: dto.root_hash.clone(),
        network_id: dto.network_id.clone(),
        signing_key_id: dto.signing_key_id.clone(),
        signature: dto.signature.clone(),
        created_at: dto.created_at,
    };
    keys::check_head(
        &key,
        &CosignedTreeHead {
            sth,
            cosignatures: Vec::new(),
        },
        bounds,
    )?;
    if pinned.is_none() {
        keys::can_admit(pool, shard_id, source_url, bounds).await?;
    }
    Ok(key)
}

/// Upper bound on one mirrored entry's payload plus identifying fields for a
/// self-certifying shard. A local willingness-to-store limit, not a validity rule.
const MAX_SELF_CERTIFYING_ENTRY_BYTES: usize = 64 * 1024;
/// Entries fetched per self-certifying shard in one tick.
const SELF_CERTIFYING_ENTRIES_PER_TICK: usize = 1000;
/// Wall-clock budget for all self-certifying backfill in one tick.
const SELF_CERTIFYING_TICK_BUDGET: Duration = Duration::from_secs(60);
/// Slack past the tick budget before a stalled self-certifying backfill is cut off.
const SELF_CERTIFYING_BACKFILL_GRACE: Duration = Duration::from_secs(10);

/// Per-shard and overall limits on one tick's backfill work.
#[derive(Clone, Copy)]
struct BackfillLimits {
    max_entries: usize,
    deadline: std::time::Instant,
}

impl BackfillLimits {
    fn for_tick(deadline: std::time::Instant) -> Self {
        Self {
            max_entries: SELF_CERTIFYING_ENTRIES_PER_TICK,
            deadline,
        }
    }

    fn exhausted(&self, processed: usize, now: std::time::Instant) -> bool {
        processed >= self.max_entries || now >= self.deadline
    }
}

/// Whether a fetched entry is small enough to store for a self-certifying shard.
fn entry_within_size_limit(entry: &LedgerEntryDto) -> bool {
    let payload = entry.payload.as_ref().map_or(0, |p| p.to_string().len());
    payload + entry.kind.len() + entry.issuer.len() + entry.subject.len()
        <= MAX_SELF_CERTIFYING_ENTRY_BYTES
}

/// Whether verified mirrored entries of `shard_id` are applied to this node's
/// indexer. Entries of a self-certifying shard are stored in the mirror tables
/// only: a shard anyone can mint must not create identities or projected state.
fn projects_mirrored_entries(shard_id: &str) -> bool {
    !avalon_protocol::shard_identity::is_self_certifying(shard_id)
}

/// Spawned once at startup (see `main.rs`) when [`MirrorWatcherConfig::from_env`]
/// returns `Some`. Never returns. See module docs for the two-phase
/// per-tick shape: every peer is polled for its STH independently first
/// (phase 1), then peers are grouped by `network_id` and backfilled
/// together (phase 2) — one unreachable/misbehaving peer never stops the
/// others from being watched or from covering for it during backfill.
///
/// Bundles [`run_worker`]'s dependencies beyond the core
/// pool/chain/indexer/config quartet — `interest`/`own_base_url`
/// register this node's own interest in every `network_id` phase 1
/// actually verifies an STH for (harmless, no-op bookkeeping if
/// `AVALON_DHT_ENABLED` is unset, see `crate::interest`'s own module doc
/// comment on that); `wake` is woken by `crate::mirror_push::notify`'s
/// handler on an incoming push notification, short-circuiting the rest of
/// the current poll interval; `shard_registry`/`own_base_url` also feed
/// [`discover_and_verify_shard_peers`]; `own_shard_id`
/// is this node's own locally-authored shard, needed so
/// [`check_equivocation`] only folds this node's own signed history into a
/// comparison for observations of *that* shard. Grouped into one struct
/// purely to keep [`run_worker`]'s own signature under clippy's
/// too-many-arguments threshold — no shared lifecycle beyond that.
pub struct MirrorWatcherHandles {
    pub interest: crate::interest::InterestRegistry,
    pub shard_registry: crate::nodes::ShardRegistry,
    pub own_base_url: Option<String>,
    pub wake: std::sync::Arc<tokio::sync::Notify>,
    pub own_shard_id: String,
    /// This node's own witness known list, read live on every
    /// verification — never a snapshot taken once at startup, so admitting
    /// or dropping a witness takes effect on this worker's very next poll
    /// tick, no restart needed. See `crate::cosign_verify`'s own module doc
    /// comment for the identity-bridging caveat.
    pub known_list: KnownListHandle,
    /// Consulted before every cosigning decision: a shard with a confirmed
    /// equivocation on record gets no further cosignatures from this node.
    pub head_gossip: HeadGossipTracker,
    /// `None` when this node has opted out of cosigning or has no witness
    /// signing key — see [`WitnessCosignConfig::from_env`].
    pub witness: Option<WitnessCosignConfig>,
    /// Peer table used to resolve a known-list witness's base URL when
    /// gathering cosignatures.
    pub peers: crate::nodes::PeerTable,
    /// Trust anchors a peer's claimed `network_id` is resolved against.
    pub trust_anchors: Vec<avalon_protocol::network_trust::TrustAnchorEntry>,
}

/// What `record_verified_head` needs to decide whether an author-verified
/// head has reached majority: this tick's known list and how to reach each
/// of its witnesses.
struct MajorityContext<'a> {
    known_list: &'a [(String, VerifyingKey)],
    sources: &'a [cosign_gather::WitnessSource],
    policy: crate::outbound_policy::OutboundPolicy,
    self_certifying: &'a crate::self_certifying_keys::MirrorBounds,
}

type HeldHeads = std::collections::HashSet<(String, i64, String)>;
const MAX_HELD_LOG_ENTRIES: usize = 1024;

/// Spawned once at startup (see `main.rs`) when [`MirrorWatcherConfig::from_env`]
/// returns `Some`. Never returns. See module docs for the two-phase
/// per-tick shape: every peer is polled for its STH independently first
/// (phase 1), then peers are grouped by `network_id`/`shard_id` and
/// backfilled together (phase 2) — one unreachable/misbehaving peer never
/// stops the others from being watched or from covering for it during
/// backfill. See [`MirrorWatcherHandles`]'s own doc comment for what
/// `handles` carries and why.
pub async fn run_worker(
    pool: PgPool,
    chain: PostgresSettlementProvider,
    indexer: PostgresIndexer,
    config: MirrorWatcherConfig,
    handles: MirrorWatcherHandles,
) {
    let MirrorWatcherHandles {
        interest,
        shard_registry,
        own_base_url,
        wake,
        own_shard_id,
        known_list,
        head_gossip,
        witness,
        peers,
        trust_anchors,
    } = handles;
    let mut held_logged = HeldHeads::new();
    let self_certifying_bounds = crate::self_certifying_keys::MirrorBounds::from_env();
    let mut gather_log = GatherLog::new();
    let mut directory_state = crate::witness_refresh::RefreshState::default();
    let directory_max = crate::witness_refresh::max_per_tick_from_env();
    let own_witness_key = witness.as_ref().map(|w| w.key_id().to_string());
    let policy = crate::outbound_policy::OutboundPolicy::from_env();
    let client = crate::node_http::NodeClient::guarded();

    if config.peers.is_empty() {
        tracing::info!(
            "mirror-watcher: no statically-configured peers (AVALON_MIRROR_PEERS unset) — \
             auto_mirror_discovered={}",
            config.auto_mirror_discovered
        );
    } else {
        let peers_display: Vec<String> = config
            .peers
            .iter()
            .map(|(shard_id, url)| format!("{shard_id}={url}"))
            .collect();
        tracing::info!(
            "mirror-watcher: watching {} peer(s) every {:?}: {} (auto_mirror_discovered={})",
            config.peers.len(),
            config.poll_interval,
            peers_display.join(", "),
            config.auto_mirror_discovered
        );
    }
    if own_base_url.is_none() {
        tracing::info!(
            "mirror-watcher: AVALON_NODE_URL is unset — this node can still receive push \
             notifications targeted at whatever interest it registers, but registered peers \
             pushing to it will only reach it once it has a reachable base URL to advertise"
        );
    }

    // Issue #596: held for the life of this worker, one guard per
    // network_id this tick's phase 1 has ever verified an STH for — never
    // dropped, since this node keeps mirroring that network for as long as
    // it's configured to. Registering here (only once an STH has actually
    // verified, never on a bare, unverified peer URL) is what keeps a
    // hostile or unpinned network_id claim from ever reaching the DHT.
    let mut network_interest: HashMap<String, crate::interest::InterestGuard> = HashMap::new();

    let mut tick = 0usize;
    loop {
        tick = tick.wrapping_add(1);
        // Read live at the top of every tick — never a snapshot taken once
        // at startup — so an admitted or dropped known-list witness is
        // reflected starting this very tick, no restart needed.
        let known_list_pairs = cosign_verify::known_list_verifying_keys(&known_list);
        let witness_sources = cosign_gather::witness_sources(&known_list_pairs, &peers.list_all());
        let majority = MajorityContext {
            known_list: &known_list_pairs,
            sources: &witness_sources,
            policy,
            self_certifying: &self_certifying_bounds,
        };

        // Phase 1: poll every peer independently for its latest STH,
        // verify + store + equivocation-check each one. Peers that
        // succeed are grouped by (network_id, shard_id), since that's
        // what phase 2 backfills over and two different
        // shards under the same network_id must never be corroborated or
        // backfilled together, even if they happen to report the same
        // tree_size.
        let mut verified_by_shard: HashMap<(String, String), Vec<(String, CosignedTreeHead)>> =
            HashMap::new();
        // New self-certifying shards accepted this tick, shared with discovery below.
        let mut admitted_new = BTreeSet::new();
        // Sources are fetched concurrently, each within a deadline, so dead ones cannot stack
        // their waits; verification and storage then run one at a time.
        let mut fetched = Vec::with_capacity(config.peers.len());
        for batch in config.peers.chunks(MAX_CONCURRENT_SOURCE_FETCHES) {
            let client = &client;
            fetched.extend(
                futures_util::future::join_all(batch.iter().map(|(shard_id, peer)| async move {
                    (
                        shard_id,
                        peer,
                        fetch_source_head(
                            client,
                            peer,
                            shard_id,
                            configured_fetch_deadline(shard_id),
                        )
                        .await,
                    )
                }))
                .await,
            );
        }
        for (shard_id, peer, fetched) in fetched {
            let verified = match fetched {
                Ok(fetched) => {
                    verify_configured_head(
                        &pool,
                        chain.network_id(),
                        &trust_anchors,
                        shard_id,
                        peer,
                        fetched,
                        &self_certifying_bounds,
                        &admitted_new,
                    )
                    .await
                }
                Err(err) => Err(err),
            };
            match verified {
                Ok(Some((head, author_key))) => {
                    let was_admitted = is_admitted(&pool, shard_id).await;
                    record_verified_head(
                        &pool,
                        &chain,
                        &client,
                        witness.as_ref(),
                        &head_gossip,
                        Some((&interest, &mut network_interest)),
                        &mut verified_by_shard,
                        &own_shard_id,
                        shard_id,
                        peer,
                        head,
                        author_key,
                        &majority,
                        &mut held_logged,
                    )
                    .await;
                    charge_admission(&pool, shard_id, was_admitted, &mut admitted_new).await;
                }
                Ok(None) => {}
                Err(err) => log_source_failure(shard_id, peer, &err),
            }
        }

        if config.auto_mirror_discovered {
            let discovered = discover_and_verify_shard_peers(
                &client,
                &pool,
                chain.network_id(),
                &shard_registry,
                own_base_url.as_deref(),
                &config.known_shard_ids,
                &self_certifying_bounds,
                &admitted_new,
            )
            .await;
            for (shard_id, peer, head, author_key) in discovered {
                // Unlike the statically-configured loop above,
                // a purely gossip-discovered shard never registers
                // push-based mirror-sync interest — discovering a shard's
                // existence is not the same as this node committing to
                // keep watching it the way an explicit
                // `AVALON_MIRROR_PEERS` entry does.
                if budget_spent(&pool, &shard_id, &self_certifying_bounds, &admitted_new).await {
                    log_budget_skip(&shard_id, &peer);
                    continue;
                }
                let was_admitted = is_admitted(&pool, &shard_id).await;
                record_verified_head(
                    &pool,
                    &chain,
                    &client,
                    witness.as_ref(),
                    &head_gossip,
                    None,
                    &mut verified_by_shard,
                    &own_shard_id,
                    &shard_id,
                    &peer,
                    head,
                    author_key,
                    &majority,
                    &mut held_logged,
                )
                .await;
                charge_admission(&pool, &shard_id, was_admitted, &mut admitted_new).await;
            }
        }

        // Refresh every known witness's own cosignature for each watched shard's latest head,
        // whether or not any source answered this tick. Pinned self-certifying shards are
        // mirrored shards too.
        let refresh_shards = shards_to_refresh(
            &config.peers,
            crate::self_certifying_keys::pinned_shard_ids(&pool).await,
        );
        let directory = crate::witness_refresh::directory_witnesses(
            &peers.list_all(),
            &known_list_pairs,
            own_witness_key.as_deref(),
            OffsetDateTime::now_utc(),
        );
        directory_state.retain_live(&directory.iter().map(|w| w.key_id.as_str()).collect());
        for shard_id in &refresh_shards {
            let mut extra = DirectoryRefresh {
                witnesses: &directory,
                max_per_tick: directory_max,
                state: &mut directory_state,
                source_url: single_source(&config.peers, shard_id),
            };
            refresh_witness_cosignatures(
                &pool,
                &chain,
                &majority,
                &mut extra,
                shard_id,
                &mut gather_log,
            )
            .await;
        }

        let backfill_deadline = std::time::Instant::now() + SELF_CERTIFYING_TICK_BUDGET;
        // Phase 2: for each (network, shard) at least one peer reported
        // this tick, pick a corroborated tree head and backfill against
        // every peer that agreed on it.
        for ((network_id, shard_id), heads) in &verified_by_shard {
            let observations: Vec<(String, SignedTreeHead)> = heads
                .iter()
                .map(|(peer, head)| (peer.clone(), head.sth.clone()))
                .collect();
            let limits = (!projects_mirrored_entries(shard_id))
                .then(|| BackfillLimits::for_tick(backfill_deadline));
            let work = backfill_network(
                &client,
                &pool,
                &indexer,
                network_id,
                shard_id,
                &observations,
                limits,
                tick,
            );
            // A self-certifying source may stall a request; the budget bounds the whole shard.
            let result = match limits {
                Some(l) => {
                    let left = l
                        .deadline
                        .saturating_duration_since(std::time::Instant::now());
                    tokio::time::timeout(left + SELF_CERTIFYING_BACKFILL_GRACE, work)
                        .await
                        .unwrap_or_else(|_| {
                            tracing::warn!(shard_id, "mirror-watcher: backfill ran past the tick budget, resuming next tick");
                            Ok(())
                        })
                }
                None => work.await,
            };
            if let Err(err) = result {
                tracing::error!("mirror-watcher: {network_id}/{shard_id}: {err}");
            }
        }

        // Issue #596: wait for whichever comes first — the normal poll
        // interval, or a push notification waking this loop early. Either
        // way the next iteration runs the exact same verify/corroborate/
        // backfill pipeline above; a push only ever changes *when* that
        // runs, never what it trusts.
        tokio::select! {
            _ = tokio::time::sleep(config.poll_interval) => {}
            _ = wake.notified() => {
                tracing::info!(
                    "mirror-watcher: woke early due to a push notification, re-polling now"
                );
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorWatcherError {
    #[error("http request failed: {0}")]
    Http(#[from] crate::node_http::NodeHttpError),
    #[error("peer returned an unparseable response: {0}")]
    Decode(String),
    #[error("STH signature verification failed — refusing to trust this observation")]
    InvalidSignature,
    /// Issue #513/#515: distinguishable from [`Self::InvalidSignature`] on
    /// purpose — there's no key to even attempt verification against,
    /// because this `network_id` has no entry in
    /// `docs/trusted-networks.json`. A node only ever mirrors networks it
    /// has an explicit, reviewed trust anchor for; an unpinned
    /// `network_id` (typo, unrelated network, or a peer just claiming
    /// whatever it wants) is refused the same way an invalid signature is,
    /// never silently stored.
    #[error("peer claims network_id {0:?}, which has no pinned trust anchor in docs/trusted-networks.json — refusing to mirror it")]
    UnpinnedNetwork(String),
    #[error("inclusion proof failed verification for seq={seq} against tree_size={tree_size}")]
    InvalidInclusionProof { seq: i64, tree_size: i64 },
    #[error("peer's inclusion-proof root_hash did not match the already-verified STH root_hash")]
    RootHashMismatch,
    #[error("entry seq={seq}: content does not hash to its claimed entry_hash")]
    EntryContentMismatch { seq: i64 },
    #[error("entry seq={seq}: prev_hash does not link to the previous verified entry")]
    EntryChainBroken { seq: i64 },
    #[error("entry seq={seq}: seq does not increase past the last mirrored seq (or jumps implausibly far)")]
    EntrySeqInvalid { seq: i64 },
    #[error("entry seq={seq}: a different entry is already mirrored at this seq")]
    EntryConflict { seq: i64 },
    #[error("entry seq={seq}: payload is pruned, so its content cannot be verified")]
    EntryPayloadPruned { seq: i64 },
    #[error("entry seq={seq} exceeds the per-entry size limit for a self-certifying shard")]
    EntryTooLarge { seq: i64 },
    #[error("self-certifying shard rejected: {0:?}")]
    SelfCertifyingRejected(crate::self_certifying_keys::Rejection),
    #[error("every configured peer for this network failed this request")]
    AllPeersFailed,
    #[error("storage error: {0}")]
    Storage(#[from] avalon_chain::SettlementError),
    /// Issue #368: distinguishable from [`Self::Decode`] on purpose — this
    /// peer's response decoded just fine, it's the reported version that
    /// doesn't clear this node's floor (or is empty/unparseable). Never a
    /// panic, and reversible: the peer's next STH is re-checked fresh on
    /// this node's next poll tick, so an upgrade is picked up automatically.
    #[error("peer reported protocol_version {peer_version:?}, below this node's effective floor {floor}")]
    IncompatiblePeerVersion { peer_version: String, floor: String },
}

#[derive(Deserialize)]
struct SignedTreeHeadDto {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    /// Issue #368: additive (`#[serde(default)]`) so an older peer's
    /// response without this field still decodes fine — it's reported as
    /// an empty string, which `crate::version::is_supported` always
    /// treats as unsupported (never given the benefit of the doubt), same
    /// as a genuinely malformed version string.
    #[serde(default)]
    protocol_version: String,
    /// Additive (`#[serde(default)]`) so an older peer's
    /// response without this field still decodes fine — an empty list,
    /// which `cosigned_sth::verify_cosigned_tree_head` treats exactly like
    /// a head this node simply hasn't collected any cosignatures for yet.
    #[serde(default)]
    cosignatures: Vec<WitnessCosignatureDto>,
    /// Hex public key of a self-certifying shard's signer; absent otherwise.
    #[serde(default)]
    signing_public_key: Option<String>,
}

impl From<SignedTreeHeadDto> for SignedTreeHead {
    fn from(dto: SignedTreeHeadDto) -> Self {
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

impl From<SignedTreeHeadDto> for CosignedTreeHead {
    fn from(dto: SignedTreeHeadDto) -> Self {
        let cosignature_dtos = dto.cosignatures.clone();
        let sth: SignedTreeHead = dto.into();
        let cosignatures = cosignature_dtos
            .iter()
            .map(|c| c.to_witness_cosignature(&sth))
            .collect();
        CosignedTreeHead { sth, cosignatures }
    }
}

#[derive(Deserialize)]
struct LedgerEntryDto {
    seq: i64,
    event_id: Uuid,
    kind: String,
    issuer: String,
    subject: String,
    payload: Option<serde_json::Value>,
    #[serde(default)]
    payload_pruned: bool,
    version: i32,
    #[serde(with = "time::serde::rfc3339")]
    event_timestamp: OffsetDateTime,
    prev_hash: String,
    entry_hash: String,
    batch_id: Uuid,
}

#[derive(Deserialize)]
struct InclusionProofDto {
    root_hash: String,
    leaf_hash: String,
    proof: Vec<String>,
}

/// Pure check, split out for direct unit testing without a live peer:
/// `Err` (never a panic) if `peer_protocol_version` doesn't clear this
/// node's effective floor — logged as a clear, distinct "incompatible"
/// event, whether the reported version is empty (an older peer, or a
/// decode that fell back to `SignedTreeHeadDto`'s `#[serde(default)]`),
/// unparseable, or simply below the floor.
fn check_peer_version(peer: &str, peer_protocol_version: &str) -> Result<(), MirrorWatcherError> {
    if crate::version::is_supported(peer_protocol_version) {
        return Ok(());
    }
    let floor = crate::version::effective_min_peer_version().to_string();
    tracing::error!(
        event = "incompatible_peer_version",
        peer = %peer,
        peer_version = %peer_protocol_version,
        floor = %floor,
        "peer's reported protocol_version is below this node's effective floor — not \
         trusting this STH; this is a compatibility/availability signal only, never a \
         security check, and is self-correcting once the peer upgrades",
    );
    Err(MirrorWatcherError::IncompatiblePeerVersion {
        peer_version: peer_protocol_version.to_string(),
        floor,
    })
}

/// Longest a `node:` shard source (configured or discovered) may take to answer a head request.
const SELF_CERTIFYING_FETCH_DEADLINE: Duration = Duration::from_secs(30);
/// Longest a core or trust-anchor source may take: longer, because such a peer may be honestly
/// rate-limited and operators depend on it; a `node:` shard operator is untrusted.
const CORE_FETCH_DEADLINE: Duration = Duration::from_secs(120);

/// The head-fetch deadline for a configured source of `shard_id`.
fn configured_fetch_deadline(shard_id: &str) -> Duration {
    if avalon_protocol::shard_identity::is_self_certifying(shard_id) {
        SELF_CERTIFYING_FETCH_DEADLINE
    } else {
        CORE_FETCH_DEADLINE
    }
}
/// Configured sources fetched at once, so dead ones cannot stack their waits.
const MAX_CONCURRENT_SOURCE_FETCHES: usize = 8;

/// Fetches `source`'s latest head for `shard_id` within `deadline`, retries included.
async fn fetch_source_head(
    client: &crate::node_http::NodeClient,
    source: &str,
    shard_id: &str,
    deadline: Duration,
) -> Result<(SignedTreeHeadDto, String), MirrorWatcherError> {
    tokio::time::timeout(deadline, fetch_latest_sth(client, source, Some(shard_id)))
        .await
        .unwrap_or_else(|_| {
            Err(MirrorWatcherError::Http(
                crate::node_http::NodeHttpError::Stream {
                    kind: crate::node_http::StreamErrorKind::Timeout,
                    message: "source did not answer in time".into(),
                },
            ))
        })
}

/// Verifies one configured source's fetched head. A self-certifying shard (`node:<hash>`) is
/// verified by its own key; every other shard against the network trust anchor. The transport
/// (http(s) or `p2p://`) does not change either path. `Ok(None)` when the per-tick
/// new-shard budget skips it.
#[allow(clippy::too_many_arguments)]
async fn verify_configured_head(
    pool: &PgPool,
    network_id: &str,
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
    shard_id: &str,
    source: &str,
    fetched: (SignedTreeHeadDto, String),
    bounds: &crate::self_certifying_keys::MirrorBounds,
    admitted_new: &BTreeSet<String>,
) -> Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError> {
    if avalon_protocol::shard_identity::is_self_certifying(shard_id) {
        return verify_fetched_self_certifying_head(
            pool,
            network_id,
            shard_id,
            source,
            fetched,
            bounds,
            admitted_new,
        )
        .await;
    }
    verify_head_against_anchors(anchors, source, fetched.0, &fetched.1).map(Some)
}

/// Whether `err` means the source could not be reached (as opposed to answering badly).
fn is_unreachable(err: &MirrorWatcherError) -> bool {
    use crate::node_http::{NodeHttpError, StreamErrorKind};
    match err {
        MirrorWatcherError::Http(NodeHttpError::Stream { kind, .. }) => {
            *kind != StreamErrorKind::Protocol
        }
        MirrorWatcherError::Http(NodeHttpError::Http(e)) => e.is_connect() || e.is_timeout(),
        _ => false,
    }
}

/// Whether a configured source's failure is logged at most once per interval. Only an
/// unreachable `node:` shard source is: a p2p shard operator may be offline for long. A core or
/// trust-anchor source failure stays an error every tick, since operators alert on it, and a
/// verification refusal is always logged.
fn failure_is_throttled(shard_id: &str, err: &MirrorWatcherError) -> bool {
    avalon_protocol::shard_identity::is_self_certifying(shard_id) && is_unreachable(err)
}

fn log_source_failure(shard_id: &str, source: &str, err: &MirrorWatcherError) {
    if !failure_is_throttled(shard_id, err) {
        tracing::error!("mirror-watcher: {source}: {err}");
    } else if let Some(held_back) = permit_log(&format!("fail:{shard_id}:{source}")) {
        tracing::warn!(held_back, "mirror-watcher: {source}: {err}");
    }
}

/// Verifies the author signature of `peer`'s already fetched latest STH against the network
/// trust anchor — the one step every peer goes through in phase 1. Majority
/// cosignature is decided afterwards by `record_verified_head`, so cosigning
/// never depends on it. Issue #604: `shard_id` is sent as an explicit `?shard_id=` query
/// param (#573's documented footgun) — a peer serving more than one shard
/// (mirroring one, authoring another) answers a bare request with whichever
/// it treats as its own default, not necessarily the one this config entry
/// means.
///
/// Returns the accepted head alongside the author key it verified against
/// — a caller cross-checking two accepted heads for equivocation
/// (`run_worker`) needs the same key both were checked against.
fn verify_head_against_anchors(
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
    peer: &str,
    dto: SignedTreeHeadDto,
    peer_protocol_version: &str,
) -> Result<(CosignedTreeHead, VerifyingKey), MirrorWatcherError> {
    check_peer_version(peer, peer_protocol_version)?;

    let Some(verify_key) = verify_key_for_network(anchors, &dto.network_id) else {
        tracing::error!(
            event = "mirror_peer_network_unpinned",
            peer = %peer,
            network_id = %dto.network_id,
            "peer claims a network_id with no pinned trust anchor — not mirroring",
        );
        return Err(MirrorWatcherError::UnpinnedNetwork(dto.network_id));
    };

    let head: CosignedTreeHead = dto.into();
    let now = OffsetDateTime::now_utc();
    let Some(matched_key) =
        cosign_verify::verify_cosigned_against_any_key([verify_key], &head, &[], now)
    else {
        tracing::error!(
            event = "sth_verification_failed",
            peer = %peer,
            network_id = %head.sth.network_id,
            tree_size = head.sth.tree_size,
            "STH failed verification: the author signature is invalid — not storing, not trusting",
        );
        return Err(MirrorWatcherError::InvalidSignature);
    };
    Ok((head, matched_key))
}

/// Resolves the verify key for whatever `network_id` a peer's STH actually
/// claims, rather than one process-wide key — the same
/// per-network trust-anchor lookup `avalon_protocol::network_trust::evaluate_network_trust`
/// uses client-side, applied here so a node mirroring peers across
/// more than one legitimate, pinned network verifies each against its own
/// correct key. `None` for a `network_id` with no entry in
/// `docs/trusted-networks.json` at all — the caller treats that as a hard
/// refusal, never a fallback to some other key. Pure, for
/// direct unit testing (same split-out pattern `nodes::resolve_bootstrap_peers`
/// already uses in this repo).
fn verify_key_for_network(
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
    network_id: &str,
) -> Option<VerifyingKey> {
    let entry = anchors.iter().find(|a| a.network_id == network_id)?;
    let bytes = hex::decode(&entry.verify_key).ok()?;
    let key_array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&key_array).ok()
}

/// Returns the decoded STH alongside the peer's separately-reported
/// `protocol_version` — kept apart from [`SignedTreeHead`] itself (see
/// `SignedTreeHeadDto`'s own doc comment): the version is never part of
/// the signed payload.
///
/// `shard_id`, when given, is sent as an explicit `?shard_id=` query
/// param — a footgun restated here since [`discover_and_verify_shard_peers`]
/// found it live: a peer serving more than one shard (mirroring one,
/// authoring another, same as `avalon-peer` in this project's own sandbox
/// topology) answers a bare `/ledger/sth/latest` with whichever shard
/// *it* treats as its own default, not necessarily the one being asked
/// about — silently fetching and then failing to verify the wrong
/// shard's STH entirely. `None` preserves the original bare-request
/// behavior for callers that already know they're talking to a
/// single-shard peer (or are deliberately asking for that peer's own
/// default).
async fn fetch_latest_sth(
    client: &crate::node_http::NodeClient,
    peer: &str,
    shard_id: Option<&str>,
) -> Result<(SignedTreeHeadDto, String), MirrorWatcherError> {
    let url = format!("{peer}/ledger/sth/latest");
    let mut request = client.get(&url);
    if let Some(shard_id) = shard_id {
        request = request.query(&[("shard_id", shard_id)]);
    }
    let dto: SignedTreeHeadDto = send_with_rate_limit_retry(request)
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    let protocol_version = dto.protocol_version.clone();
    Ok((dto, protocol_version))
}

/// Sends `request`, retrying with backoff on HTTP 429 — live
/// testing surfaced this codebase's own rate limiter as a real
/// concern for backfill, not just a hypothetical: backfilling a real,
/// busy history's worth of individual `/ledger/entries`/`/ledger/proof/inclusion`
/// requests can legitimately exceed `AVALON_RATE_LIMIT_PER_MINUTE`
/// mid-backfill, and treating that as an ordinary peer failure (this
/// tick's backfill just aborts, retried whole-hog next poll interval)
/// makes backfilling any sufficiently large history painfully slow at
/// best. Respects a `Retry-After` header when the rate limiter sends one
/// (`GovernorLayer` does), else falls back to a short fixed
/// backoff. Bounded to a handful of attempts so a persistently
/// misbehaving/adversarial peer still surfaces as a real failure rather
/// than retrying forever — the existing per-tick retry (next poll
/// interval) is still the ultimate backstop.
const MIN_RETRY_AFTER: Duration = Duration::from_millis(500);
/// A source's `Retry-After` is never honoured beyond this; a hostile one cannot stall the loop.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// How long to wait after a 429 carrying `header` (the `Retry-After` value), clamped.
fn retry_wait(header: Option<&str>) -> Duration {
    header
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(MIN_RETRY_AFTER, Duration::from_secs)
        .clamp(MIN_RETRY_AFTER, MAX_RETRY_AFTER)
}

async fn send_with_rate_limit_retry(
    request: crate::node_http::NodeRequestBuilder,
) -> Result<crate::node_http::NodeResponse, crate::node_http::NodeHttpError> {
    const MAX_RATE_LIMIT_RETRIES: u32 = 5;

    let mut attempt = 0u32;
    loop {
        let this_attempt = request
            .try_clone()
            .expect("GET requests built here never stream a body, so cloning always succeeds");
        let response = this_attempt.send().await?;
        if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS
            || attempt >= MAX_RATE_LIMIT_RETRIES
        {
            return Ok(response);
        }
        // Issue #604: a `Retry-After: 0` (this rate limiter's GCRA
        // algorithm regenerates tokens continuously, so "0 whole seconds"
        // is a real, common answer, not "retry immediately") still needs
        // a real, non-zero floor — retrying in an actual tight loop just
        // re-hits the same still-exhausted token bucket.
        let wait = retry_wait(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
        );
        attempt += 1;
        tracing::warn!(
            attempt,
            wait_secs = wait.as_secs_f64(),
            "mirror-watcher: rate-limited (429) fetching from a peer, backing off and retrying"
        );
        tokio::time::sleep(wait).await;
    }
}

/// Durably stores every cosignature attached to `head` that
/// this node can itself vouch for — signature-valid against a key its own
/// known list currently recognizes. Cosignatures from a witness this node
/// doesn't (yet, or ever) recognize are never stored: an unbounded stream
/// of junk cosignatures from an adversarial peer would otherwise grow this
/// table forever for no benefit, and this node has no way to tell a
/// legitimate-but-unrecognized witness from a fabricated one anyway.
/// Best-effort — a storage failure here never fails verification of the
/// head itself, which already succeeded before this is ever called; a
/// cosignature this node fails to persist is simply not available to
/// re-serve to further mirrors, not a correctness problem for this node's
/// own view.
async fn store_valid_cosignatures(
    chain: &PostgresSettlementProvider,
    shard_id: &str,
    head: &CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
) {
    for cosig in &head.cosignatures {
        let Some((_, verifying_key)) = known_list
            .iter()
            .find(|(witness_key_id, _)| *witness_key_id == cosig.witness_key_id)
        else {
            continue;
        };
        if !avalon_protocol::witness::verify_witness_cosignature(verifying_key, cosig) {
            continue;
        }
        if let Err(err) = chain.store_witness_cosignature(shard_id, cosig).await {
            tracing::warn!(
                witness_key_id = %cosig.witness_key_id,
                shard_id,
                error = %err,
                "mirror-watcher: failed to durably store a valid witness cosignature",
            );
        }
    }
}

/// Pins (or refreshes) the key of an accepted self-certifying shard's head;
/// `false` when a bound refuses it.
async fn pin_accepted_shard(
    pool: &PgPool,
    shard_id: &str,
    source_url: &str,
    key: &VerifyingKey,
    majority: &MajorityContext<'_>,
) -> bool {
    match crate::self_certifying_keys::pin_key(
        pool,
        shard_id,
        key,
        source_url,
        majority.self_certifying,
    )
    .await
    {
        Ok(()) => true,
        Err(rejection) => {
            tracing::warn!(
                shard_id,
                ?rejection,
                "mirror-watcher: not mirroring a self-certifying shard"
            );
            false
        }
    }
}

/// Shards whose cosignatures are refreshed each tick: the configured mirror shards plus every
/// pinned self-certifying shard, sorted and deduplicated.
fn shards_to_refresh(configured: &[(String, String)], pinned: Vec<String>) -> Vec<String> {
    let mut shards: Vec<String> = configured.iter().map(|(s, _)| s.clone()).collect();
    shards.extend(pinned);
    shards.sort_unstable();
    shards.dedup();
    shards
}

/// Last gather outcome label per (network, shard, witness), so a witness is logged only when
/// its outcome changes.
type GatherLog = HashMap<(String, String, String), String>;

/// Witnesses known from the peer directory but outside the known list, refreshed within
/// per-tick and stored-row bounds.
struct DirectoryRefresh<'a> {
    witnesses: &'a [crate::witness_refresh::DirectoryWitness],
    max_per_tick: usize,
    state: &'a mut crate::witness_refresh::RefreshState,
    /// The single configured mirror source of the shard, scoping served-head reads like the
    /// serve path does (`ShardMirrorSources::source_url_for`).
    source_url: Option<&'a str>,
}

/// The one distinct source configured for `shard_id`, `None` when none or several are.
fn single_source<'a>(peers: &'a [(String, String)], shard_id: &str) -> Option<&'a str> {
    let mut urls: Vec<&str> = peers
        .iter()
        .filter(|(s, _)| s == shard_id)
        .map(|(_, u)| u.as_str())
        .collect();
    urls.sort_unstable();
    urls.dedup();
    match urls.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// The observation of the head this node serves for `shard_id`: the one whose root equals the
/// Merkle root of its backfilled entries, as `GET /ledger/sth/latest` recomputes it. `source_url`
/// scopes the reads exactly as that path does. `None` when that is the latest head itself,
/// nothing is backfilled, or no observation matches.
async fn served_head_observation(
    pool: &PgPool,
    state: &mut crate::witness_refresh::RefreshState,
    shard_id: &str,
    source_url: Option<&str>,
    latest: &mirror::ObservedSth,
) -> Option<mirror::ObservedSth> {
    let network_id = &latest.network_id;
    let progress = mirror::mirrored_progress(pool, network_id, shard_id, source_url)
        .await
        .ok()?;
    let count = progress.verified_count;
    if count <= 0 || count >= latest.tree_size {
        return None;
    }
    let last_hash = mirror::mirrored_entry_hash_at(pool, network_id, shard_id, progress.last_seq)
        .await
        .ok()??;
    let key = (network_id.clone(), shard_id.to_string());
    let root = match state.served_roots.get(&key) {
        Some(c) if c.count == count && c.last_hash == last_hash => c.root.clone(),
        _ => {
            let hashes =
                mirror::mirrored_entry_hashes_up_to(pool, network_id, shard_id, count, source_url)
                    .await
                    .ok()?;
            if (hashes.len() as i64) < count {
                return None;
            }
            let root = hex::encode(merkle::mth_of_hex_hashes(&hashes).ok()?);
            state.served_roots.insert(
                key,
                crate::witness_refresh::ServedRoot {
                    count,
                    last_hash,
                    root: root.clone(),
                },
            );
            root
        }
    };
    mirror::observed_sth_matching_root(pool, network_id, shard_id, count, &root, source_url)
        .await
        .ok()
        .flatten()
}

/// Asks every known-list witness, then a bounded selection of directory witnesses outside the
/// list, for its own current cosignature over the latest head observed for `shard_id` and stores
/// those that verify. Runs every tick independent of whether the head's source answered or the
/// head changed, so stored copies of other witnesses' cosignatures stay fresh while the author is
/// unreachable. A fetched cosignature that is not newer than the stored one is not stored.
async fn refresh_witness_cosignatures(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    majority: &MajorityContext<'_>,
    extra: &mut DirectoryRefresh<'_>,
    shard_id: &str,
    gather_log: &mut GatherLog,
) {
    let observed = match mirror::latest_observed_sths_for_shard(pool, shard_id).await {
        Ok(observed) => observed,
        Err(err) => {
            tracing::error!(shard_id, error = %err, "mirror-watcher: cosignature refresh could not read the latest observed head");
            return;
        }
    };
    let mut targets = Vec::new();
    for obs in observed {
        let served =
            served_head_observation(pool, extra.state, shard_id, extra.source_url, &obs).await;
        targets.push((obs, true));
        targets.extend(served.map(|o| (o, false)));
    }
    // The served head, when older than the latest, is only topped up: witnesses with a fresh
    // stored copy are skipped, and its backoff and rotation live apart from the latest head's.
    for (obs, is_latest) in targets {
        let network_id = obs.network_id.clone();
        let sth: SignedTreeHead = obs.into();
        let stored = chain
            .list_witness_cosignatures(&network_id, shard_id, sth.tree_size)
            .await
            .unwrap_or_default();
        let now = OffsetDateTime::now_utc();
        let held: HashMap<String, OffsetDateTime> = stored
            .iter()
            .filter(|c| c.root_hash == sth.root_hash)
            .map(|c| (c.witness_key_id.clone(), c.observed_at))
            .collect();
        let scope = if is_latest {
            shard_id.to_string()
        } else {
            format!("{shard_id}#served")
        };
        let needs_refresh = |id: &str| {
            is_latest
                || held
                    .get(id)
                    .is_none_or(|at| now - *at >= crate::witness_refresh::refresh_after())
        };
        let known: Vec<(String, VerifyingKey)> = majority
            .known_list
            .iter()
            .filter(|(id, _)| {
                needs_refresh(id) && (is_latest || extra.state.backoff.ready(id, &scope, now))
            })
            .cloned()
            .collect();
        let mut results = cosign_gather::gather_own_cosignatures(
            majority.policy,
            &known,
            majority.sources,
            &sth,
            shard_id,
        )
        .await;
        if !is_latest {
            for (key_id, outcome) in &results {
                let attempt = crate::witness_refresh::attempt_for(outcome, now);
                extra.state.backoff.record(key_id, &scope, attempt, now);
            }
        }
        let outside_known_list = stored
            .iter()
            .filter(|c| {
                !majority
                    .known_list
                    .iter()
                    .any(|(id, _)| *id == c.witness_key_id)
            })
            .count();
        let candidates: Vec<crate::witness_refresh::DirectoryWitness> = extra
            .witnesses
            .iter()
            .filter(|w| needs_refresh(&w.key_id))
            .cloned()
            .collect();
        let selected = crate::witness_refresh::select(
            &candidates,
            &scope,
            &held,
            outside_known_list,
            extra.state,
            extra.max_per_tick,
            now,
        );
        if !selected.is_empty() {
            let pairs: Vec<(String, VerifyingKey)> =
                selected.iter().map(|w| (w.key_id.clone(), w.key)).collect();
            let sources: Vec<cosign_gather::WitnessSource> =
                selected.iter().map(|w| w.source()).collect();
            let extra_results = cosign_gather::gather_own_cosignatures(
                majority.policy,
                &pairs,
                &sources,
                &sth,
                shard_id,
            )
            .await;
            let deliver_cap = extra.max_per_tick * crate::witness_refresh::ROW_CAP_FACTOR;
            for (key_id, outcome) in &extra_results {
                let attempt = crate::witness_refresh::attempt_for(outcome, now);
                extra.state.backoff.record(key_id, &scope, attempt, now);
                if is_latest {
                    extra.state.asked(key_id, shard_id, now);
                    if attempt == crate::witness_refresh::Attempt::Ok {
                        extra.state.delivered(key_id, shard_id, deliver_cap);
                    }
                }
            }
            results.extend(extra_results);
        }
        for (witness_key_id, outcome) in results {
            let stored_age = stored
                .iter()
                .find(|c| c.root_hash == sth.root_hash && c.witness_key_id == witness_key_id)
                .map(|c| (now - c.observed_at).whole_seconds());
            let (label, fetched_age) = match &outcome {
                cosign_gather::GatherOutcome::Fetched(cosig) => {
                    let age = (now - cosig.observed_at).whole_seconds();
                    if !crate::witness_refresh::observed_at_in_range(cosig.observed_at, now) {
                        ("observed_at out of range".to_string(), Some(age))
                    } else if held
                        .get(&witness_key_id)
                        .is_some_and(|at| *at >= cosig.observed_at)
                    {
                        ("not newer".to_string(), Some(age))
                    } else {
                        match chain.store_witness_cosignature(shard_id, cosig).await {
                            Ok(()) => ("stored".to_string(), Some(age)),
                            Err(err) => (format!("store refused: {err}"), Some(age)),
                        }
                    }
                }
                other => (other.label(), None),
            };
            let key = (
                network_id.clone(),
                shard_id.to_string(),
                witness_key_id.clone(),
            );
            let changed = gather_log.get(&key) != Some(&label);
            let short = &witness_key_id[..witness_key_id.len().min(8)];
            if changed {
                tracing::info!(
                    shard_id,
                    tree_size = sth.tree_size,
                    witness = short,
                    outcome = %label,
                    stored_age_secs = ?stored_age,
                    fetched_age_secs = ?fetched_age,
                    "mirror-watcher: witness cosignature gather outcome changed",
                );
                gather_log.insert(key, label);
            } else {
                tracing::debug!(
                    shard_id,
                    tree_size = sth.tree_size,
                    witness = short,
                    outcome = %label,
                    stored_age_secs = ?stored_age,
                    fetched_age_secs = ?fetched_age,
                    "mirror-watcher: witness cosignature gather",
                );
            }
        }
    }
}

/// The shared bookkeeping every head accepted by
/// [`verify_head_against_anchors`] or [`discover_and_verify_shard_peers`] goes
/// through — recording the raw observation (feeding the original
/// source-based equivocation detection, unchanged by cosigning), durably
/// storing whatever cosignatures this node can itself vouch for, checking
/// for a genuine *witness*-cosigned equivocation against every other head
/// already accepted this tick for the same network/shard/tree_size, and
/// finally recording the head for phase 2's corroboration/backfill
/// decision. `interest` is `Some` only for the statically-configured peer
/// loop — see that call site's own comment for why a purely
/// gossip-discovered shard never registers push interest.
#[allow(clippy::too_many_arguments)]
async fn record_verified_head(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    client: &crate::node_http::NodeClient,
    witness: Option<&WitnessCosignConfig>,
    head_gossip: &HeadGossipTracker,
    interest: Option<(
        &crate::interest::InterestRegistry,
        &mut HashMap<String, crate::interest::InterestGuard>,
    )>,
    verified_by_shard: &mut HashMap<(String, String), Vec<(String, CosignedTreeHead)>>,
    own_shard_id: &str,
    shard_id: &str,
    peer: &str,
    head: CosignedTreeHead,
    author_key: VerifyingKey,
    majority: &MajorityContext<'_>,
    held_logged: &mut HeldHeads,
) {
    // A self-certifying shard is pinned once its head is accepted. With no
    // majority to wait for that is immediate, so pin before storing anything.
    // Known gap: with a majority to wait for, the observation and cosignature below are written
    // before the pin, so a refused pin still leaves them; they are bounded and unpinned.
    let self_certifying = avalon_protocol::shard_identity::is_self_certifying(shard_id);
    let pin_first = self_certifying && majority.known_list.len() <= 1;
    if pin_first && !pin_accepted_shard(pool, shard_id, peer, &author_key, majority).await {
        return;
    }
    let observed = ObservedSth::from_sth(peer, shard_id, &head.sth, OffsetDateTime::now_utc());
    match mirror::insert_observation(pool, &observed).await {
        Ok(is_new) => {
            if is_new {
                if let Err(err) = check_equivocation(pool, chain, own_shard_id, &observed).await {
                    tracing::error!("mirror-watcher: {peer}: {err}");
                }
            }
        }
        Err(err) => {
            tracing::error!("mirror-watcher: {peer}: {err}");
            return;
        }
    }

    // Cosigning depends only on the author signature (checked by the caller)
    // plus the consistency and double-cosign guards, never on majority.
    witness_cosign::decide_and_cosign(
        chain,
        pool,
        client,
        witness,
        head_gossip,
        peer,
        shard_id,
        &head,
    )
    .await;

    let held_key = (
        shard_id.to_string(),
        head.sth.tree_size,
        head.sth.root_hash.clone(),
    );
    let head = match cosign_gather::resolve_majority(
        majority.policy,
        &author_key,
        head,
        majority.known_list,
        majority.sources,
        shard_id,
    )
    .await
    {
        cosign_gather::HeadVerdict::Trusted(head) => head,
        cosign_gather::HeadVerdict::Held => {
            if held_logged.len() >= MAX_HELD_LOG_ENTRIES {
                held_logged.clear();
            }
            if held_logged.insert(held_key.clone()) {
                tracing::info!(
                    peer = %peer,
                    shard_id,
                    tree_size = held_key.1,
                    "mirror-watcher: head's author signature is valid but cosignatures do not \
                     reach majority yet — holding it, retrying next tick",
                );
            }
            return;
        }
    };
    let known_list = majority.known_list;
    if self_certifying
        && !pin_first
        && !pin_accepted_shard(pool, shard_id, peer, &author_key, majority).await
    {
        return;
    }

    store_valid_cosignatures(chain, shard_id, &head, known_list).await;

    // A genuinely provable *witness* equivocation — only meaningful once
    // real cosigning was actually required to accept a head at all
    // (`known_list.len() > 1`; at or below that, both heads were only ever
    // checked against the bare author signature per
    // `verify_cosigned_tree_head`'s own degenerate case, which the
    // source-based `check_equivocation` above already covers as an author
    // equivocation, not a witness one).
    if known_list.len() > 1 {
        if let Some(existing_heads) =
            verified_by_shard.get(&(head.sth.network_id.clone(), shard_id.to_string()))
        {
            let now = OffsetDateTime::now_utc();
            let freshness_cutoff = now - cosign_verify::COSIGNATURE_FRESHNESS_WINDOW;
            for (_, other_head) in existing_heads {
                let equivocators = avalon_protocol::cosigned_sth::find_equivocating_witnesses(
                    &author_key,
                    known_list,
                    freshness_cutoff,
                    now,
                    other_head,
                    &head,
                );
                if !equivocators.is_empty() {
                    let evidence = mirror::WitnessEquivocationEvidence {
                        kind: mirror::EquivocationEvidenceKind::Witness,
                        network_id: head.sth.network_id.clone(),
                        shard_id: shard_id.to_string(),
                        tree_size: head.sth.tree_size,
                        head_a: other_head.clone(),
                        head_b: head.clone(),
                        equivocating_witness_key_ids: equivocators,
                        detected_at: None,
                    };
                    if let Err(err) =
                        mirror::record_witness_equivocation_evidence(pool, &evidence).await
                    {
                        tracing::error!(
                            "mirror-watcher: {peer}: failed to durably record witness \
                             equivocation evidence: {err}"
                        );
                    }
                }
            }
        }
    }

    if let Some((interest, network_interest)) = interest {
        network_interest
            .entry(head.sth.network_id.clone())
            .or_insert_with(|| {
                tracing::info!(
                    network_id = %head.sth.network_id,
                    "mirror-watcher: registering interest for push-based mirror sync (issue #596)"
                );
                interest.register(crate::interest::InterestScope::for_network(
                    &head.sth.network_id,
                ))
            });
    }
    verified_by_shard
        .entry((head.sth.network_id.clone(), shard_id.to_string()))
        .or_default()
        .push((peer.to_string(), head));
}

/// Compares `observed` against every other observation this node has
/// recorded at the same `network_id`/`tree_size` — from other peers *and*
/// (if this node is also a Settlement authority) its own signed history —
/// and durably records/logs any disagreement found. See
/// `avalon_chain::mirror`'s module docs for the surfacing mechanism. This
/// ticket owns detection only: there is no "pick the correct STH" logic
/// here or anywhere else in this module — both STHs in a real equivocation
/// are validly signed, so there is no automatically-correct answer.
async fn check_equivocation(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    own_shard_id: &str,
    observed: &ObservedSth,
) -> Result<(), MirrorWatcherError> {
    let mut existing = mirror::observations_at(
        pool,
        &observed.network_id,
        &observed.shard_id,
        observed.tree_size,
    )
    .await?;

    // Fold in this node's own signed history, if it has one at this exact
    // tree_size — a node that is both an authority and a mirror must catch
    // itself disagreeing with what it broadcasts, not just catch two peers
    // disagreeing with each other. Issue #604: only when `observed` is
    // actually about this node's own authored shard — folding it in for a
    // *different* shard this node happens to also be mirroring would
    // compare across two unrelated logs, exactly the bug class this
    // ticket fixes.
    if chain.network_id() == observed.network_id && own_shard_id == observed.shard_id {
        if let Some(own) = chain.signed_tree_head_at(observed.tree_size).await? {
            existing.push(ObservedSth::from_sth(
                SELF_SIGNED_SOURCE,
                own_shard_id,
                &own,
                own.created_at,
            ));
        }
    }

    for finding in mirror::detect_equivocation(&existing, observed) {
        mirror::record_equivocation(pool, &finding).await?;
    }

    Ok(())
}

/// Phase 2 for one `network_id`: decide whether — and against what tree
/// head — to backfill this tick, given every peer that successfully
/// reported an STH for this network this tick.
///
/// Two gates, both before a single byte of entry content is trusted:
///
/// 1. **Equivocation gate.** If this network already has *any* recorded
///    equivocation finding, backfill refuses to proceed at all. This
///    ticket owns detection, not resolution — once two validly signed
///    STHs disagree for this network, there is no automatically-correct
///    tree to keep extending, and continuing to backfill against whichever
///    peer happens to answer would silently pick a side. A human has to
///    resolve this (tracked as a separate follow-up); this node just stops
///    advancing until that happens.
/// 2. **Corroboration gate.** Among this tick's successfully-polled peers,
///    pick the `(tree_size, root_hash)` the largest number of them agree
///    on — not simply whichever peer happened to be first in the config
///    list. A peer reporting a different, smaller `tree_size` is just
///    behind, not disagreeing, and isn't counted against the winner.
#[allow(clippy::too_many_arguments)]
async fn backfill_network(
    client: &crate::node_http::NodeClient,
    pool: &PgPool,
    indexer: &PostgresIndexer,
    network_id: &str,
    shard_id: &str,
    observations: &[(String, SignedTreeHead)],
    limits: Option<BackfillLimits>,
    tick: usize,
) -> Result<(), MirrorWatcherError> {
    let equivocations = mirror::unresolved_equivocations(pool, network_id, shard_id).await?;
    if !equivocations.is_empty() {
        tracing::error!(
            event = "equivocation_backfill_blocked",
            network_id = %network_id,
            shard_id = %shard_id,
            unresolved_findings = equivocations.len(),
            "refusing to backfill — unresolved equivocation finding(s) recorded for this shard; needs human investigation before further backfill can be trusted",
        );
        return Ok(());
    }

    let projection_blocked = reproject_unapplied(pool, indexer, network_id, shard_id)
        .await?
        .blocked;

    let mut agreement: HashMap<(i64, &str), Vec<&str>> = HashMap::new();
    for (peer, sth) in observations {
        agreement
            .entry((sth.tree_size, sth.root_hash.as_str()))
            .or_default()
            .push(peer.as_str());
    }
    let Some(((target_tree_size, target_root_hash), agreeing_peers)) =
        agreement.into_iter().max_by_key(|(_, peers)| peers.len())
    else {
        return Ok(());
    };
    if agreeing_peers.len() > 1 {
        tracing::info!(
            "mirror-watcher: {network_id}: tree_size={target_tree_size} corroborated by {} of {} polled peer(s)",
            agreeing_peers.len(),
            observations.len()
        );
    }

    let target_sth = observations
        .iter()
        .find(|(_, s)| s.tree_size == target_tree_size && s.root_hash == target_root_hash)
        .map(|(_, s)| s.clone())
        .expect("target_tree_size/target_root_hash were derived from this exact list");

    let candidate_peers: Vec<String> = agreeing_peers.iter().map(|p| p.to_string()).collect();
    backfill(
        client,
        pool,
        indexer,
        shard_id,
        &candidate_peers,
        &target_sth,
        limits,
        projection_blocked,
        tick,
    )
    .await
}

/// Fetches and independently verifies every entry between what's already
/// been mirrored locally and `sth.tree_size`, storing only entries whose
/// inclusion actually checks out. Starts at a rotating one of `candidate_peers`
/// (every peer that corroborated `sth` this tick, per [`backfill_network`]);
/// each page's entries and proofs come from one candidate, and a candidate
/// that is unreachable, serves nothing or serves an entry that fails
/// verification is excluded for the tick and the page retried from the next.
/// Storage is keyed on
/// `(network_id, shard_id, seq)`, so failing over between peers within the
/// same shard never duplicates or restarts progress, and two different
/// shards' entries — even at the same `seq` — are never confused for each
/// other (see `avalon_chain::mirror::insert_mirrored_entry`'s
/// doc comment).
///
/// Aborts (without storing anything further this pass) the moment *every*
/// candidate peer's response fails to verify for a given entry — the
/// invariant this ticket calls out explicitly: never trust unverified
/// content from a peer, and a partial-but-unverifiable backfill is worse
/// than simply retrying next tick.
///
/// Issue #313: once an entry's inclusion is verified, it's decoded into the
/// same `ProtocolEvent` shape `outbox::drain_once` builds and applied to
/// this node's own local `PostgresIndexer` — in the same transaction as the
/// `mirrored_entries` write, so a remote-settlement node's local reads are
/// fed *only* by content this node independently verified itself, never by
/// trusting a peer's response or (for the write side) `POST
/// /ledger/submit`'s request body.
#[allow(clippy::too_many_arguments)]
async fn backfill(
    client: &crate::node_http::NodeClient,
    pool: &PgPool,
    indexer: &PostgresIndexer,
    shard_id: &str,
    candidate_peers: &[String],
    sth: &SignedTreeHead,
    limits: Option<BackfillLimits>,
    projection_blocked: bool,
    tick: usize,
) -> Result<(), MirrorWatcherError> {
    if candidate_peers.is_empty() {
        return Ok(());
    }

    // Issue #604: `shard_id` scopes this to exactly the shard `sth`
    // belongs to — #573 left this deliberately unscoped ("how much of
    // this *network* have I mirrored"), explicitly naming this as a real,
    // separate follow-up question at the time. Confirmed live as a real
    // bug, not just theoretical: a node mirroring more than one shard of
    // the same network had its inclusion-proof verification state
    // silently collide between shards without this.
    let progress = mirror::mirrored_progress(pool, &sth.network_id, shard_id, None).await?;
    if progress.verified_count >= sth.tree_size {
        return Ok(());
    }

    let expected_root = hex::decode(&sth.root_hash)
        .map_err(|e| MirrorWatcherError::Decode(format!("STH root_hash not valid hex: {e}")))?;
    let mut root = [0u8; 32];
    root.copy_from_slice(&expected_root);

    // The next entry must link to this hash: the last stored entry's, or genesis.
    let prev_hash = match progress.verified_count {
        0 => avalon_chain::GENESIS_HASH.to_string(),
        _ => mirror::mirrored_entry_hash_at(pool, &sth.network_id, shard_id, progress.last_seq)
            .await?
            .ok_or_else(|| {
                MirrorWatcherError::Decode("last mirrored entry vanished mid-backfill".into())
            })?,
    };

    // Each tick starts at a different candidate so no single peer takes all the traffic.
    let n = candidate_peers.len();
    let mut peer_cursor = tick % n;
    // A candidate that served bad or no data is skipped for the rest of this tick.
    let mut excluded = vec![false; n];
    let mut last_refusal: Option<MirrorWatcherError> = None;
    let mut state = BackfillState {
        progress,
        prev_hash,
        fetched: 0,
        projection_blocked,
    };

    loop {
        if state.progress.verified_count >= sth.tree_size {
            break;
        }
        if limits.is_some_and(|l| l.exhausted(state.fetched, std::time::Instant::now())) {
            tracing::info!(
                shard_id,
                "mirror-watcher: per-tick backfill budget spent, resuming next tick"
            );
            break;
        }
        let Some(idx) = (0..n)
            .map(|o| (peer_cursor + o) % n)
            .find(|i| !excluded[*i])
        else {
            return match last_refusal {
                Some(err) => Err(err),
                None => {
                    tracing::error!(
                        "mirror-watcher: {}: no candidate peer could serve the next entries after seq={} — retrying next tick",
                        sth.network_id, state.progress.last_seq
                    );
                    Ok(())
                }
            };
        };
        let peer = &candidate_peers[idx];
        match backfill_page(
            client, pool, indexer, shard_id, peer, sth, &root, limits, &mut state,
        )
        .await
        {
            Ok(PageEnd::Served) => peer_cursor = (idx + 1) % n,
            Ok(PageEnd::Empty) => {
                tracing::error!(
                    "mirror-watcher: {} (via {peer}): served no entries after seq={} although the verified head holds {} (mirrored {}) — trying another candidate (or a lagging source; retried next tick)",
                    sth.network_id, state.progress.last_seq, sth.tree_size, state.progress.verified_count
                );
                excluded[idx] = true;
            }
            Err(PageError::Unreachable(err)) => {
                tracing::error!("mirror-watcher: {} (via {peer}): request failed, trying the next candidate peer: {err}", sth.network_id);
                excluded[idx] = true;
            }
            Err(PageError::Refused(err)) => {
                tracing::error!("mirror-watcher: {} (via {peer}): refused ({err}) — nothing stored from the refused entry, excluding this source for the rest of the tick", sth.network_id);
                excluded[idx] = true;
                last_refusal = Some(err);
            }
            Err(PageError::Fatal(err)) => return Err(err),
        }
    }

    Ok(())
}

/// Running state of one [`backfill`] tick.
struct BackfillState {
    progress: mirror::MirrorProgress,
    /// Hash the next entry's `prev_hash` must equal.
    prev_hash: String,
    fetched: usize,
    projection_blocked: bool,
}

/// How one page from one candidate ended without an error.
enum PageEnd {
    Served,
    Empty,
}

/// Why a page from one candidate stopped. `Refused` and `Unreachable` fail over to the next
/// candidate; `Fatal` (local storage) ends the tick.
enum PageError {
    Refused(MirrorWatcherError),
    Unreachable(MirrorWatcherError),
    Fatal(MirrorWatcherError),
}

/// Most a stored `seq` may jump past the previous one. Rolled-back commits burn identity values,
/// so gaps are legitimate and can be large; the bound only stops absurd labels that would strand
/// the since_seq cursor (2^32 is far beyond any real ledger).
const MAX_SEQ_GAP: i64 = 1 << 32;

/// Classifies a failed proof request: a transport failure or throttling is an outage, while a
/// 4xx for an entry the source just served or a body that does not decode is a refusal.
fn proof_failure(err: MirrorWatcherError) -> PageError {
    let refused = match &err {
        MirrorWatcherError::Decode(_) => true,
        MirrorWatcherError::Http(crate::node_http::NodeHttpError::Decode(_)) => true,
        MirrorWatcherError::Http(crate::node_http::NodeHttpError::Status(s)) => {
            s.is_client_error() && *s != reqwest::StatusCode::TOO_MANY_REQUESTS
        }
        MirrorWatcherError::Http(crate::node_http::NodeHttpError::Http(e)) => {
            e.is_decode()
                || e.status().is_some_and(|s| {
                    s.is_client_error() && s != reqwest::StatusCode::TOO_MANY_REQUESTS
                })
        }
        _ => false,
    };
    if refused {
        PageError::Refused(err)
    } else {
        PageError::Unreachable(err)
    }
}

/// The payload to hash: absent only when the source says it was pruned, a served JSON null
/// is a real payload.
fn served_payload(entry: &LedgerEntryDto) -> Option<serde_json::Value> {
    if entry.payload_pruned {
        return None;
    }
    Some(entry.payload.clone().unwrap_or(serde_json::Value::Null))
}

/// Fetches one page from `peer` and verifies and stores its entries in order; the entries and
/// their proofs both come from this one candidate.
#[allow(clippy::too_many_arguments)]
async fn backfill_page(
    client: &crate::node_http::NodeClient,
    pool: &PgPool,
    indexer: &PostgresIndexer,
    shard_id: &str,
    peer: &str,
    sth: &SignedTreeHead,
    root: &[u8; 32],
    limits: Option<BackfillLimits>,
    state: &mut BackfillState,
) -> Result<PageEnd, PageError> {
    let page = fetch_entries(
        client,
        peer,
        shard_id,
        state.progress.last_seq,
        BACKFILL_PAGE_SIZE,
    )
    .await
    .map_err(PageError::Unreachable)?;
    if page.is_empty() {
        return Ok(PageEnd::Empty);
    }
    for entry in page {
        if state.progress.verified_count >= sth.tree_size {
            break;
        }
        if let Some(l) = limits {
            if l.exhausted(state.fetched, std::time::Instant::now()) {
                break;
            }
            if !entry_within_size_limit(&entry) {
                tracing::warn!(shard_id, seq = entry.seq, "mirror-watcher: entry exceeds the per-entry size limit, not mirroring this shard further");
                return Err(PageError::Refused(MirrorWatcherError::EntryTooLarge {
                    seq: entry.seq,
                }));
            }
        }
        state.fetched += 1;

        // seq is not covered by the entry hash, so it must at least keep the stored order.
        let last_seq = state.progress.last_seq;
        if entry.seq > last_seq && entry.seq - last_seq > MAX_SEQ_GAP {
            tracing::error!(
                "mirror-watcher: {} (via {peer}): entry seq={} jumps past the last mirrored seq={last_seq} by more than the bound {MAX_SEQ_GAP} — refusing it and excluding this source for the tick; mirror from another source or investigate this one",
                sth.network_id, entry.seq
            );
        }
        if entry.seq <= last_seq || entry.seq - last_seq > MAX_SEQ_GAP {
            return Err(PageError::Refused(MirrorWatcherError::EntrySeqInvalid {
                seq: entry.seq,
            }));
        }

        // The leaf's rank among committed entries: how many this node has already accepted,
        // which holds because backfill proceeds in order with no entry skipped.
        let leaf_index = state.progress.verified_count as usize;
        let proof_dto = fetch_inclusion_proof(client, peer, shard_id, entry.seq, sth.tree_size)
            .await
            .map_err(proof_failure)?;
        if proof_dto.root_hash != sth.root_hash {
            return Err(PageError::Refused(MirrorWatcherError::RootHashMismatch));
        }

        let mirrored_entry = mirror::MirroredEntry {
            source_url: peer.to_string(),
            network_id: sth.network_id.clone(),
            shard_id: shard_id.to_string(),
            seq: entry.seq,
            event_id: entry.event_id,
            kind: entry.kind.clone(),
            issuer: entry.issuer.clone(),
            subject: entry.subject.clone(),
            payload: served_payload(&entry),
            event_timestamp: entry.event_timestamp,
            version: entry.version,
            prev_hash: entry.prev_hash.clone(),
            entry_hash: entry.entry_hash.clone(),
            batch_id: entry.batch_id,
            verified_tree_size: sth.tree_size,
        };
        let invalid_proof = || {
            PageError::Refused(MirrorWatcherError::InvalidInclusionProof {
                seq: mirrored_entry.seq,
                tree_size: sth.tree_size,
            })
        };
        if proof_dto.leaf_hash != mirrored_entry.entry_hash {
            return Err(invalid_proof());
        }
        verify_entry_binding(&mirrored_entry, &proof_dto.leaf_hash, &state.prev_hash)
            .map_err(PageError::Refused)?;

        let leaf_bytes = hex::decode(&mirrored_entry.entry_hash).map_err(|e| {
            PageError::Refused(MirrorWatcherError::Decode(format!(
                "entry_hash not valid hex: {e}"
            )))
        })?;
        let proof_nodes = decode_proof_nodes(&proof_dto.proof).map_err(PageError::Refused)?;
        if !merkle::verify_inclusion_proof(
            &leaf_bytes,
            leaf_index,
            sth.tree_size as usize,
            &proof_nodes,
            root,
        ) {
            return Err(invalid_proof());
        }

        store_and_project(
            pool,
            indexer,
            &mirrored_entry,
            &mut state.projection_blocked,
        )
        .await
        .map_err(PageError::Fatal)?;
        state.progress.last_seq = mirrored_entry.seq;
        state.progress.verified_count += 1;
        state.prev_hash = mirrored_entry.entry_hash;
    }
    Ok(PageEnd::Served)
}

/// Binds a fetched entry's content to the verified chain: its content must hash to the claimed
/// `entry_hash`, which must equal the proof leaf, and its `prev_hash` must be the previous
/// verified entry's hash. A pruned payload cannot be recomputed and is refused.
fn verify_entry_binding(
    entry: &mirror::MirroredEntry,
    proof_leaf: &str,
    expected_prev_hash: &str,
) -> Result<(), MirrorWatcherError> {
    let seq = entry.seq;
    if entry.prev_hash != expected_prev_hash {
        return Err(MirrorWatcherError::EntryChainBroken { seq });
    }
    let recomputed = entry
        .recomputed_hash()
        .ok_or(MirrorWatcherError::EntryPayloadPruned { seq })?;
    if recomputed != entry.entry_hash || recomputed != proof_leaf {
        return Err(MirrorWatcherError::EntryContentMismatch { seq });
    }
    Ok(())
}

/// How one projection attempt ended. `Permanent` failures are deterministic
/// for this entry; `Transient` ones (storage unavailable) may succeed later.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectionOutcome {
    Applied,
    /// Nothing to project (pruned payload or malformed ids): not a failure,
    /// and no reason to park or rescan.
    Skipped,
    Permanent(String),
    Transient(String),
}

/// Entries that failed permanently this process; later passes skip them so
/// they cannot block the entries behind them. Cleared by a restart, which
/// retries them (a register may then apply after its revoke; accepted).
static PARKED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
    std::sync::LazyLock::new(Default::default);
const MAX_PARKED: usize = 1024;

fn park_in(parked: &mut std::collections::HashSet<Uuid>, event_id: Uuid, cap: usize) -> bool {
    (parked.len() < cap && parked.insert(event_id)) || parked.contains(&event_id)
}

fn park_entry(event_id: Uuid) {
    let parked = park_in(
        &mut PARKED.lock().unwrap_or_else(|e| e.into_inner()),
        event_id,
        MAX_PARKED,
    );
    if !parked {
        tracing::warn!(
            "mirror-watcher: parked-entry cap ({MAX_PARKED}) reached; event_id={event_id} will be retried on every full scan"
        );
    }
}

fn is_parked(event_id: &Uuid) -> bool {
    PARKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(event_id)
}

/// `(network, shard)` pairs whose last full unapplied-entry scan was clean.
/// A pair absent here (process start, or after any projection failure) gets a
/// full anti-join scan; a present one does no scan work at all.
static CLEAN_SCANS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(String, String)>>,
> = std::sync::LazyLock::new(Default::default);

fn scan_due(network_id: &str, shard_id: &str) -> bool {
    !CLEAN_SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&(network_id.to_string(), shard_id.to_string()))
}

fn mark_scan_clean(network_id: &str, shard_id: &str) {
    CLEAN_SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((network_id.to_string(), shard_id.to_string()));
}

fn mark_projection_failed(network_id: &str, shard_id: &str) {
    CLEAN_SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(network_id.to_string(), shard_id.to_string()));
}

/// Stores a verified entry and, unless an earlier projection this tick is
/// blocked, projects it in the same transaction. A blocked entry stays
/// unapplied so the ordered re-projection applies it after its predecessors.
async fn store_and_project(
    pool: &PgPool,
    indexer: &PostgresIndexer,
    entry: &mirror::MirroredEntry,
    projection_blocked: &mut bool,
) -> Result<(), MirrorWatcherError> {
    let storage = |e: sqlx::Error| avalon_chain::SettlementError::Storage(e.to_string());
    let mut tx = pool.begin().await.map_err(storage)?;
    if !mirror::insert_mirrored_entry(&mut *tx, entry).await? {
        return Err(MirrorWatcherError::EntryConflict { seq: entry.seq });
    }
    if projects_mirrored_entries(&entry.shard_id) {
        if *projection_blocked {
            mark_projection_failed(&entry.network_id, &entry.shard_id);
        } else {
            match project_mirrored_entry(&mut tx, indexer, entry).await? {
                ProjectionOutcome::Applied | ProjectionOutcome::Skipped => {}
                ProjectionOutcome::Permanent(_) => {
                    park_entry(entry.event_id);
                    mark_projection_failed(&entry.network_id, &entry.shard_id);
                }
                ProjectionOutcome::Transient(_) => {
                    *projection_blocked = true;
                    mark_projection_failed(&entry.network_id, &entry.shard_id);
                }
            }
        }
    }
    tx.commit().await.map_err(storage)?;
    Ok(())
}

/// Applies one verified mirrored entry to the local indexer inside a savepoint
/// of `tx`. A failure rolls back only the savepoint (so the entry stays
/// unclaimed in `indexer_applied_events`), is logged, and is classified.
async fn project_mirrored_entry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    indexer: &PostgresIndexer,
    entry: &mirror::MirroredEntry,
) -> Result<ProjectionOutcome, MirrorWatcherError> {
    let storage = |e: sqlx::Error| avalon_chain::SettlementError::Storage(e.to_string());
    let Some(event) = protocol_event_from_mirrored(entry) else {
        tracing::warn!(
            "mirror-watcher: {}: event_id={} seq={} cannot be decoded (payload pruned or issuer/subject malformed) — mirrored, not applied to the local indexer",
            entry.network_id, entry.event_id, entry.seq
        );
        return Ok(ProjectionOutcome::Skipped);
    };
    let mut savepoint = tx.begin().await.map_err(storage)?;
    let applied = async {
        ensure_identity_row_exists(&mut savepoint, &event).await?;
        indexer.apply_in_tx(&mut savepoint, &event).await
    }
    .await;
    match applied {
        Ok(()) => {
            savepoint.commit().await.map_err(storage)?;
            Ok(ProjectionOutcome::Applied)
        }
        Err(err) => {
            savepoint.rollback().await.map_err(storage)?;
            let reason = err.to_string();
            let (kind, outcome) = if err.is_transient() {
                ("transient", ProjectionOutcome::Transient(reason))
            } else {
                ("permanent", ProjectionOutcome::Permanent(reason))
            };
            tracing::error!(
                "mirror-watcher: {}: event_id={} seq={} verified and mirrored, but the local indexer projection failed ({kind}): {err}",
                entry.network_id, entry.event_id, entry.seq
            );
            Ok(outcome)
        }
    }
}

const REPROJECT_BATCH: i64 = 500;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ReprojectReport {
    projected: usize,
    parked: usize,
    /// A transient failure stopped the pass; later entries were left unapplied.
    blocked: bool,
}

/// Re-drives projection for stored entries of `shard_id` that the indexer
/// never claimed (a failed projection, or a node upgraded past a projection
/// bug), oldest first. Does a full anti-join scan only the first time per
/// shard after process start and after a projection failure; otherwise it is
/// a no-op. A transient failure stops the pass to keep order; a permanent one
/// parks the entry (see [`PARKED`]) and continues.
async fn reproject_unapplied(
    pool: &PgPool,
    indexer: &PostgresIndexer,
    network_id: &str,
    shard_id: &str,
) -> Result<ReprojectReport, MirrorWatcherError> {
    reproject_with(pool, network_id, shard_id, |entry| async move {
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;
        let outcome = project_mirrored_entry(&mut tx, indexer, &entry).await?;
        tx.commit()
            .await
            .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;
        Ok(outcome)
    })
    .await
}

async fn reproject_with<F, Fut>(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    mut project: F,
) -> Result<ReprojectReport, MirrorWatcherError>
where
    F: FnMut(mirror::MirroredEntry) -> Fut,
    Fut: std::future::Future<Output = Result<ProjectionOutcome, MirrorWatcherError>>,
{
    let mut report = ReprojectReport::default();
    if !projects_mirrored_entries(shard_id) || !scan_due(network_id, shard_id) {
        return Ok(report);
    }
    let storage = |e: sqlx::Error| avalon_chain::SettlementError::Storage(e.to_string());
    let mut cursor = 0i64;
    loop {
        let rows = sqlx::query(
            "SELECT m.source_url, m.network_id, m.shard_id, m.seq, m.event_id, m.kind, \
                    m.issuer, m.subject, m.payload, m.event_timestamp, m.version, \
                    m.prev_hash, m.entry_hash, m.batch_id, m.verified_tree_size \
             FROM mirrored_entries m \
             WHERE m.network_id = $1 AND m.shard_id = $2 AND m.seq > $3 \
               AND m.payload IS NOT NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM indexer_applied_events a WHERE a.event_id = m.event_id) \
             ORDER BY m.seq LIMIT $4",
        )
        .bind(network_id)
        .bind(shard_id)
        .bind(cursor)
        .bind(REPROJECT_BATCH)
        .fetch_all(pool)
        .await
        .map_err(storage)?;
        let fetched = rows.len() as i64;
        for row in rows {
            let entry = mirror::mirrored_entry_from_row(row)?;
            cursor = entry.seq;
            if is_parked(&entry.event_id) {
                continue;
            }
            let event_id = entry.event_id;
            let outcome = match project(entry).await {
                Ok(outcome) => outcome,
                Err(err) => {
                    mark_projection_failed(network_id, shard_id);
                    return Err(err);
                }
            };
            match outcome {
                ProjectionOutcome::Applied => report.projected += 1,
                ProjectionOutcome::Skipped => {}
                ProjectionOutcome::Permanent(_) => {
                    park_entry(event_id);
                    report.parked += 1;
                    mark_projection_failed(network_id, shard_id);
                }
                ProjectionOutcome::Transient(_) => {
                    report.blocked = true;
                    mark_projection_failed(network_id, shard_id);
                    return Ok(report);
                }
            }
        }
        if fetched < REPROJECT_BATCH {
            break;
        }
    }
    mark_scan_clean(network_id, shard_id);
    if report.projected > 0 {
        tracing::info!(
            "mirror-watcher: {network_id}: re-projected {} previously unapplied entr(ies) of shard {shard_id}",
            report.projected
        );
    }
    Ok(report)
}

/// Decodes a verified `MirroredEntry` into the same `ProtocolEvent` shape
/// `outbox::drain_once` builds from `protocol_outbox` rows.
/// `None` for a pruned payload (retention — the entry is still
/// mirrored, just not applicable to the indexer) or an `issuer`/`subject`
/// that isn't a well-formed `GlobalId`, which should never happen for a
/// genuine ledger entry but is handled as a skip, not a panic, since this is
/// peer-derived content.
fn protocol_event_from_mirrored(entry: &mirror::MirroredEntry) -> Option<ProtocolEvent> {
    let (payload, identity_chain) =
        avalon_protocol::identity_chain_wire::split_position(entry.payload.clone()?);
    Some(ProtocolEvent {
        id: entry.event_id,
        kind: entry.kind.clone(),
        issuer: global_id_from_str(&entry.issuer)?,
        subject: global_id_from_str(&entry.subject)?,
        payload,
        timestamp: entry.event_timestamp,
        version: u32::try_from(entry.version).ok()?,
        identity_chain,
    })
}

/// `GlobalId` derives `Deserialize` as a transparent newtype over `String`,
/// so this is the same round trip a `ProtocolEvent`'s `issuer`/`subject`
/// field already goes through in every other JSON boundary in this crate —
/// there is no public raw-string constructor on `GlobalId` itself.
fn global_id_from_str(raw: &str) -> Option<GlobalId> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

/// The identity row an `identity.created` v2 event may create: its payload id and inception
/// key, only when the id is derived from the key and matches the id embedded in issuer and subject.
fn identity_row_target(event: &ProtocolEvent) -> Option<(IdentityId, [u8; 32])> {
    if event.kind != "identity.created" || event.version != 2 {
        return None;
    }
    let created: IdentityCreatedPayload = serde_json::from_value(event.payload.clone()).ok()?;
    let key = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &created.public_key,
    )
    .ok()
    .and_then(|raw| <[u8; 32]>::try_from(raw).ok())
    .filter(|key| {
        avalon_protocol::ed25519_key::parse_ed25519_public_key(key).is_some()
            && created.identity_id.matches_key(key)
    })?;
    let embedded = |g: &GlobalId| g.as_str().split(':').nth(1).map(str::to_owned);
    let want = created.identity_id.to_string();
    (embedded(&event.issuer).as_deref() == Some(want.as_str())
        && embedded(&event.subject).as_deref() == Some(want.as_str()))
    .then_some((created.identity_id, key))
}

/// Idempotently inserts the `identities` row an `identity.created` event describes, so a
/// replay-only node (which never ran the local registration) can project the identity's history.
async fn ensure_identity_row_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: &ProtocolEvent,
) -> Result<(), avalon_indexer::IndexError> {
    if event.kind != "identity.created" {
        return Ok(());
    }
    let Some((identity_id, inception_key)) = identity_row_target(event) else {
        tracing::warn!(
            "mirror-watcher: event {} ({}) is not a valid v2 identity creation; not creating an identity row",
            event.id, event.kind
        );
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO identities (id, created_at, inception_public_key) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(identity_id)
    .bind(event.timestamp)
    .bind(inception_key.as_slice())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn decode_proof_nodes(hex_nodes: &[String]) -> Result<Vec<[u8; 32]>, MirrorWatcherError> {
    hex_nodes
        .iter()
        .map(|hex_node| {
            let bytes = hex::decode(hex_node).map_err(|e| {
                MirrorWatcherError::Decode(format!("proof node not valid hex: {e}"))
            })?;
            <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| MirrorWatcherError::Decode("proof node was not 32 bytes".to_string()))
        })
        .collect()
}

/// Issue #604: `shard_id` sent as an explicit query param, same #573
/// footgun as [`fetch_latest_sth`] — a peer serving more than one shard
/// answers a bare request with whichever it treats as its own default.
async fn fetch_entries(
    client: &crate::node_http::NodeClient,
    peer: &str,
    shard_id: &str,
    since_seq: i64,
    limit: i64,
) -> Result<Vec<LedgerEntryDto>, MirrorWatcherError> {
    let url = format!("{peer}/ledger/entries");
    let request = client.get(&url).query(&[
        ("since_seq", since_seq.to_string()),
        ("limit", limit.to_string()),
        ("shard_id", shard_id.to_string()),
    ]);
    let entries: Vec<LedgerEntryDto> = send_with_rate_limit_retry(request)
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    Ok(entries)
}

/// Issue #604: `shard_id` sent as an explicit query param — same reason
/// as [`fetch_entries`].
async fn fetch_inclusion_proof(
    client: &crate::node_http::NodeClient,
    peer: &str,
    shard_id: &str,
    seq: i64,
    tree_size: i64,
) -> Result<InclusionProofDto, MirrorWatcherError> {
    let url = format!("{peer}/ledger/proof/inclusion");
    let request = client.get(&url).query(&[
        ("seq", seq.to_string()),
        ("tree_size", tree_size.to_string()),
        ("shard_id", shard_id.to_string()),
    ]);
    let dto: InclusionProofDto = send_with_rate_limit_retry(request)
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    Ok(dto)
}

#[cfg(test)]
mod p2p_source_tests;

#[cfg(test)]
mod entry_binding_tests;

#[cfg(test)]
mod real_authority_tests;

#[cfg(test)]
mod tests {
    fn seed_anchor(
        network_id: &str,
        seeds: &[&str],
    ) -> avalon_protocol::network_trust::TrustAnchorEntry {
        avalon_protocol::network_trust::TrustAnchorEntry {
            label: network_id.to_string(),
            network_id: network_id.to_string(),
            verify_key: "ab".repeat(32),
            signing_key_id: "k".to_string(),
            server_url: None,
            environment: avalon_protocol::network_trust::NetworkEnvironment::Dev,
            seed_nodes: seeds.iter().map(|s| s.to_string()).collect(),
            notes: None,
        }
    }

    #[test]
    fn default_core_mirror_uses_seed_nodes_for_a_non_core_author() {
        let anchors = vec![seed_anchor("net", &["http://a:1", "http://b:2"])];
        for shard in ["", "game:x"] {
            let got = resolve_default_core_mirror_peers(shard, None, "net", &anchors);
            assert_eq!(got, "http://a:1,http://b:2");
        }
    }

    #[test]
    fn default_core_mirror_is_empty_when_explicit_core_author_or_no_seeds() {
        let anchors = vec![
            seed_anchor("net", &["http://a:1"]),
            seed_anchor("bare", &[]),
        ];
        assert_eq!(
            resolve_default_core_mirror_peers("", Some("http://x"), "net", &anchors),
            ""
        );
        assert_eq!(
            resolve_default_core_mirror_peers("core", None, "net", &anchors),
            ""
        );
        assert_eq!(
            resolve_default_core_mirror_peers("", None, "bare", &anchors),
            ""
        );
        assert_eq!(
            resolve_default_core_mirror_peers("", None, "unknown", &anchors),
            ""
        );
    }

    #[test]
    fn blank_explicit_mirror_peers_still_defaults() {
        let anchors = vec![seed_anchor("net", &["http://a:1"])];
        assert_eq!(
            resolve_default_core_mirror_peers("", Some("  "), "net", &anchors),
            "http://a:1"
        );
    }

    use super::*;
    use avalon_protocol::identity_id::TestIdentity;

    #[test]
    fn a_peer_at_the_current_protocol_version_passes() {
        assert!(check_peer_version("http://peer", crate::version::PROTOCOL_VERSION).is_ok());
    }

    #[test]
    fn an_empty_peer_version_is_incompatible_not_a_panic() {
        let err = check_peer_version("http://peer", "").unwrap_err();
        assert!(matches!(
            err,
            MirrorWatcherError::IncompatiblePeerVersion { .. }
        ));
    }

    #[test]
    fn an_unparseable_peer_version_is_incompatible_not_a_panic() {
        let err = check_peer_version("http://peer", "not-a-version").unwrap_err();
        assert!(matches!(
            err,
            MirrorWatcherError::IncompatiblePeerVersion { .. }
        ));
    }

    #[test]
    fn a_peer_version_below_the_floor_is_incompatible() {
        let err = check_peer_version("http://peer", "0.0.1").unwrap_err();
        match err {
            MirrorWatcherError::IncompatiblePeerVersion {
                peer_version,
                floor,
            } => {
                assert_eq!(peer_version, "0.0.1");
                assert_eq!(
                    floor,
                    crate::version::effective_min_peer_version().to_string()
                );
            }
            other => panic!("expected IncompatiblePeerVersion, got {other:?}"),
        }
    }

    fn anchor(
        network_id: &str,
        verify_key_hex: String,
    ) -> avalon_protocol::network_trust::TrustAnchorEntry {
        avalon_protocol::network_trust::TrustAnchorEntry {
            label: network_id.to_string(),
            network_id: network_id.to_string(),
            verify_key: verify_key_hex,
            signing_key_id: "test-key".to_string(),
            server_url: None,
            environment: avalon_protocol::network_trust::NetworkEnvironment::LocalDev,
            seed_nodes: Vec::new(),
            notes: None,
        }
    }

    // Issue #515: a node mirroring peers on two distinct, independently
    // pinned networks must resolve each against its own key, not one
    // shared process-wide key.
    #[test]
    fn verify_key_for_network_resolves_the_matching_pinned_entry_among_several() {
        let key_a = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let key_b = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let anchors = vec![
            anchor("avalon-a", hex::encode(key_a.verifying_key().to_bytes())),
            anchor("avalon-b", hex::encode(key_b.verifying_key().to_bytes())),
        ];

        let resolved = verify_key_for_network(&anchors, "avalon-b").expect("pinned");
        assert_eq!(resolved, key_b.verifying_key());
    }

    // Issue #513: a `network_id` with no pinned trust anchor must never
    // fall back to some other key — there is simply nothing to verify
    // against, so mirroring it is refused outright.
    #[test]
    fn verify_key_for_network_returns_none_for_an_unpinned_network() {
        let key_a = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let anchors = vec![anchor(
            "avalon-a",
            hex::encode(key_a.verifying_key().to_bytes()),
        )];

        assert!(verify_key_for_network(&anchors, "avalon-unpinned").is_none());
    }

    #[test]
    fn verify_key_for_network_returns_none_for_a_malformed_pinned_key() {
        let anchors = vec![anchor("avalon-a", "not-hex".to_string())];

        assert!(verify_key_for_network(&anchors, "avalon-a").is_none());
    }

    // Both cases live in one test (rather than two `#[test]` fns) because
    // `cargo test` runs tests in the same binary concurrently by default,
    // and both cases mutate the same process-wide `AVALON_MIRROR_PEERS` env
    // var — two separate tests racing on it would be flaky.
    // All `AVALON_MIRROR_PEERS`/`AVALON_MIRROR_ALL_DISCOVERED_SHARDS`
    // cases live in this one test (rather than separate `#[test]` fns),
    // same reasoning as the comment above: `cargo test` runs tests in one
    // binary concurrently by default, and every case here mutates the
    // same process-wide env vars — separate tests racing on them would be
    // flaky.
    #[test]
    fn from_env_reads_peers_and_poll_interval() {
        let _env = crate::test_env::guard();
        // SAFETY: test-only env mutation of vars no other test in this
        // binary touches.
        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
            std::env::remove_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS");
        }
        assert!(MirrorWatcherConfig::from_env().is_none());

        unsafe {
            std::env::set_var(
                "AVALON_MIRROR_PEERS",
                "http://localhost:8081/, http://localhost:8082",
            );
            std::env::remove_var("AVALON_MIRROR_POLL_INTERVAL_SECS");
        }
        let config = MirrorWatcherConfig::from_env().expect("peers were set");
        assert_eq!(
            config.peers,
            vec![
                ("core".to_string(), "http://localhost:8081".to_string()),
                ("core".to_string(), "http://localhost:8082".to_string()),
            ]
        );
        assert_eq!(
            config.poll_interval,
            Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS)
        );
        assert!(!config.auto_mirror_discovered);
        assert!(
            config.known_shard_ids.contains("core"),
            "a bare, un-prefixed AVALON_MIRROR_PEERS entry is the implicit core shard"
        );

        // Issue #599: `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` is enough
        // on its own — no `AVALON_MIRROR_PEERS` required — since a node
        // should be able to opt into auto-mirroring with zero explicit
        // peer configuration at all.
        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
            std::env::set_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS", "true");
        }
        let config =
            MirrorWatcherConfig::from_env().expect("auto_mirror_discovered alone should be enough");
        assert!(config.peers.is_empty());
        assert!(config.auto_mirror_discovered);
        assert!(config.known_shard_ids.is_empty());

        // Issue #599: `known_shard_ids` picks up a `shard_id=url` entry's
        // shard_id, not just its url.
        unsafe {
            std::env::set_var(
                "AVALON_MIRROR_PEERS",
                "game:ashen-realms=http://shard-a, http://bare-core-peer",
            );
            std::env::remove_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS");
        }
        let config = MirrorWatcherConfig::from_env().expect("peers were set");
        assert!(config.known_shard_ids.contains("game:ashen-realms"));
        assert!(config.known_shard_ids.contains("core"));

        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
            std::env::remove_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS");
        }
    }

    #[test]
    fn corroboration_picks_the_tree_head_the_most_peers_agree_on() {
        // Pure re-creation of `backfill_network`'s agreement-tallying
        // logic, without the network/DB calls around it — three peers
        // agree on one root_hash, one disagrees; the majority must win,
        // and the count must reflect three, not four.
        let sth_a = |root: &str| SignedTreeHead {
            tree_size: 10,
            root_hash: root.to_string(),
            network_id: "avalon-test".to_string(),
            signing_key_id: "k".to_string(),
            signature: "sig".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
        };
        let observations = vec![
            ("peer-a".to_string(), sth_a("aa")),
            ("peer-b".to_string(), sth_a("aa")),
            ("peer-c".to_string(), sth_a("aa")),
            ("peer-d".to_string(), sth_a("bb")),
        ];

        let mut agreement: HashMap<(i64, &str), Vec<&str>> = HashMap::new();
        for (peer, sth) in &observations {
            agreement
                .entry((sth.tree_size, sth.root_hash.as_str()))
                .or_default()
                .push(peer.as_str());
        }
        let ((_, winning_root), winners) = agreement
            .into_iter()
            .max_by_key(|(_, peers)| peers.len())
            .unwrap();

        assert_eq!(winning_root, "aa");
        assert_eq!(winners.len(), 3);
    }

    fn sample_mirrored_entry() -> mirror::MirroredEntry {
        mirror::MirroredEntry {
            source_url: "http://peer".to_string(),
            network_id: "avalon-test".to_string(),
            shard_id: mirror::CORE_SHARD_ID.to_string(),
            seq: 1,
            event_id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
            subject: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
            payload: Some(serde_json::json!({"display_name": "test"})),
            event_timestamp: OffsetDateTime::UNIX_EPOCH,
            version: 1,
            prev_hash: "aa".repeat(32),
            entry_hash: "bb".repeat(32),
            batch_id: Uuid::new_v4(),
            verified_tree_size: 1,
        }
    }

    #[test]
    fn decodes_a_verified_mirrored_entry_into_the_same_protocol_event_shape_the_outbox_builds() {
        let entry = sample_mirrored_entry();
        let event = protocol_event_from_mirrored(&entry).expect("well-formed entry decodes");

        assert_eq!(event.id, entry.event_id);
        assert_eq!(event.kind, entry.kind);
        assert_eq!(event.issuer.as_str(), entry.issuer);
        assert_eq!(event.subject.as_str(), entry.subject);
        assert_eq!(event.payload, entry.payload.unwrap());
        assert_eq!(event.timestamp, entry.event_timestamp);
        assert_eq!(event.version, entry.version as u32);
    }

    #[test]
    fn a_pruned_payload_decodes_to_none_rather_than_a_fabricated_event() {
        let mut entry = sample_mirrored_entry();
        entry.payload = None;

        assert!(protocol_event_from_mirrored(&entry).is_none());
    }

    async fn live_test_pool() -> PgPool {
        avalon_devenv::load();
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        sqlx::postgres::PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("failed to connect to Postgres — is it reachable?")
    }

    // -- self-certifying shard discovery --

    fn signed_head_body(
        signing: &ed25519_dalek::SigningKey,
        network_id: &str,
        protocol_version: &str,
    ) -> serde_json::Value {
        let sth = avalon_protocol::sth::sign_tree_head(
            signing,
            "k",
            3,
            &"ab".repeat(32),
            network_id,
            OffsetDateTime::now_utc(),
        );
        serde_json::json!({
            "tree_size": sth.tree_size,
            "root_hash": sth.root_hash,
            "network_id": sth.network_id,
            "signing_key_id": sth.signing_key_id,
            "signature": sth.signature,
            "created_at": sth.created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
            "protocol_version": protocol_version,
            "signing_public_key": hex::encode(signing.verifying_key().to_bytes()),
        })
    }

    async fn serve_head(body: serde_json::Value) -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/ledger/sth/latest"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    async fn row_count(pool: &PgPool, table: &str, shard_id: &str) -> i64 {
        let sql = match table {
            "self_certifying_shard_keys" => {
                "SELECT count(*) FROM self_certifying_shard_keys WHERE shard_id = $1"
            }
            "observed_sths" => "SELECT count(*) FROM observed_sths WHERE shard_id = $1",
            "witness_cosignatures" => {
                "SELECT count(*) FROM witness_cosignatures WHERE shard_id = $1"
            }
            _ => "SELECT count(*) FROM mirrored_entries WHERE shard_id = $1",
        };
        sqlx::query_scalar(sql)
            .bind(shard_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn discover_one(
        pool: &PgPool,
        network_id: &str,
        shard_id: &str,
        url: &str,
        bounds: &crate::self_certifying_keys::MirrorBounds,
    ) -> Vec<(String, String, CosignedTreeHead, VerifyingKey)> {
        let registry = crate::nodes::ShardRegistry::new();
        registry.record_own(shard_id, url, OffsetDateTime::now_utc());
        discover_and_verify_shard_peers(
            &crate::node_http::NodeClient::new(),
            pool,
            network_id,
            &registry,
            None,
            &BTreeSet::new(),
            bounds,
            &BTreeSet::new(),
        )
        .await
    }

    async fn nothing_landed(pool: &PgPool, shard_id: &str) {
        for table in [
            "self_certifying_shard_keys",
            "observed_sths",
            "witness_cosignatures",
            "mirrored_entries",
        ] {
            assert_eq!(row_count(pool, table, shard_id).await, 0, "{table}");
        }
    }

    #[tokio::test]
    #[ignore]
    async fn a_self_certifying_head_for_a_foreign_network_is_rejected_without_side_effects() {
        let pool = live_test_pool().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let shard_id =
            avalon_protocol::shard_identity::derive_self_certifying_id(&signing.verifying_key());
        let identities_before: i64 = sqlx::query_scalar("SELECT count(*) FROM identities")
            .fetch_one(&pool)
            .await
            .unwrap();
        let bounds = crate::self_certifying_keys::MirrorBounds::default();

        let foreign = serve_head(signed_head_body(
            &signing,
            "some-other-network",
            crate::version::PROTOCOL_VERSION,
        ))
        .await;
        let found = discover_one(
            &pool,
            "the-local-network",
            &shard_id,
            &foreign.uri(),
            &bounds,
        )
        .await;
        assert!(found.is_empty());
        nothing_landed(&pool, &shard_id).await;
        let identities_after: i64 = sqlx::query_scalar("SELECT count(*) FROM identities")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(identities_before, identities_after);

        // Same head for the local network is returned, so the rejection above is
        // due to the network id alone; even then nothing is pinned yet.
        let local = serve_head(signed_head_body(
            &signing,
            "the-local-network",
            crate::version::PROTOCOL_VERSION,
        ))
        .await;
        let found =
            discover_one(&pool, "the-local-network", &shard_id, &local.uri(), &bounds).await;
        assert_eq!(found.len(), 1);
        nothing_landed(&pool, &shard_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn a_self_certifying_head_from_an_unsupported_peer_version_is_rejected() {
        let pool = live_test_pool().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let shard_id =
            avalon_protocol::shard_identity::derive_self_certifying_id(&signing.verifying_key());
        let old = serve_head(signed_head_body(&signing, "net", "")).await;
        let found = discover_one(
            &pool,
            "net",
            &shard_id,
            &old.uri(),
            &crate::self_certifying_keys::MirrorBounds::default(),
        )
        .await;
        assert!(found.is_empty());
        nothing_landed(&pool, &shard_id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn unpinned_self_certifying_shards_examined_per_tick_are_limited() {
        let pool = live_test_pool().await;
        let bounds = crate::self_certifying_keys::MirrorBounds {
            max_new_per_tick: 2,
            ..crate::self_certifying_keys::MirrorBounds::default()
        };
        let registry = crate::nodes::ShardRegistry::new();
        let mut servers = Vec::new();
        for i in 0..4u8 {
            let mut seed = [i; 32];
            seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
            let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
            let id = avalon_protocol::shard_identity::derive_self_certifying_id(
                &signing.verifying_key(),
            );
            let server = serve_head(signed_head_body(
                &signing,
                "net",
                crate::version::PROTOCOL_VERSION,
            ))
            .await;
            registry.record_own(&id, &server.uri(), OffsetDateTime::now_utc());
            servers.push(server);
        }
        let found = discover_and_verify_shard_peers(
            &crate::node_http::NodeClient::new(),
            &pool,
            "net",
            &registry,
            None,
            &BTreeSet::new(),
            &bounds,
            &BTreeSet::new(),
        )
        .await;
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn refresh_covers_configured_and_pinned_shards_once_each() {
        let configured = vec![
            ("core".to_string(), "http://a".to_string()),
            ("core".to_string(), "http://b".to_string()),
        ];
        let got = shards_to_refresh(&configured, vec!["node:x".into(), "core".into()]);
        assert_eq!(got, vec!["core".to_string(), "node:x".to_string()]);
    }

    #[test]
    fn self_certifying_entries_are_not_projected_but_named_shards_are() {
        assert!(!projects_mirrored_entries(&format!(
            "node:{}",
            "a".repeat(64)
        )));
        assert!(projects_mirrored_entries("game:some-game"));
        assert!(projects_mirrored_entries("core"));
    }

    #[test]
    fn backfill_limits_stop_at_the_entry_budget_and_the_deadline() {
        let now = std::time::Instant::now();
        let limits = BackfillLimits {
            max_entries: 3,
            deadline: now + Duration::from_secs(10),
        };
        assert!(!limits.exhausted(2, now));
        assert!(limits.exhausted(3, now));
        assert!(limits.exhausted(0, now + Duration::from_secs(10)));
    }

    #[test]
    fn oversized_self_certifying_entries_are_refused() {
        let entry = |payload: serde_json::Value| LedgerEntryDto {
            seq: 1,
            event_id: Uuid::nil(),
            kind: "k".into(),
            issuer: "i".into(),
            subject: "s".into(),
            payload: Some(payload),
            payload_pruned: false,
            version: 1,
            event_timestamp: OffsetDateTime::UNIX_EPOCH,
            prev_hash: String::new(),
            entry_hash: String::new(),
            batch_id: Uuid::nil(),
        };
        assert!(entry_within_size_limit(&entry(
            serde_json::json!({"a": "b"})
        )));
        let big = "x".repeat(MAX_SELF_CERTIFYING_ENTRY_BYTES);
        assert!(!entry_within_size_limit(&entry(
            serde_json::json!({ "a": big })
        )));
    }

    /// Repeated refresh ticks keep every witness's stored cosignature current for an unchanged
    /// observed head with no source involved, never move one backwards, and never replace it
    /// with a cosignature over a different root.
    #[tokio::test]
    #[ignore]
    async fn refresh_keeps_other_witnesses_cosignatures_current_without_a_source() {
        use avalon_protocol::sth::sign_tree_head;
        use avalon_protocol::witness::sign_witness_cosignature;
        use ed25519_dalek::SigningKey;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let pool = live_test_pool().await;
        let network_id = format!("avalon-test-refresh-{}", Uuid::new_v4());
        let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());
        let author = SigningKey::from_bytes(&[9u8; 32]);
        let created_at = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
        let root = hex::encode([1u8; 32]);
        let sth = sign_tree_head(&author, "op", 5, &root, &network_id, created_at);
        mirror::insert_observation(
            &pool,
            &ObservedSth::from_sth("http://127.0.0.1:1", "core", &sth, created_at),
        )
        .await
        .unwrap();

        let keys: Vec<(SigningKey, String)> = [1u8, 2]
            .iter()
            .map(|s| {
                let k = SigningKey::from_bytes(&[*s; 32]);
                let id = hex::encode(k.verifying_key().to_bytes());
                (k, id)
            })
            .collect();
        let known_list: Vec<(String, VerifyingKey)> = keys
            .iter()
            .map(|(k, id)| (id.clone(), k.verifying_key()))
            .collect();
        let cosign_at = |k: &SigningKey, id: &str, at: OffsetDateTime| {
            sign_witness_cosignature(k, id, 5, &root, &network_id, created_at, at)
        };
        let mut servers = Vec::new();
        let mut sources = Vec::new();
        for (_, id) in &keys {
            let server = MockServer::start().await;
            sources.push(cosign_gather::WitnessSource {
                key_id: id.clone(),
                base_url: server.uri(),
            });
            servers.push(server);
        }
        let majority = MajorityContext {
            known_list: &known_list,
            sources: &sources,
            policy: crate::outbound_policy::OutboundPolicy::new(true),
            self_certifying: &crate::self_certifying_keys::MirrorBounds::default(),
        };
        let mut log = GatherLog::new();
        let base = OffsetDateTime::now_utc() - time::Duration::seconds(300);
        for tick in 0..3i64 {
            for (server, (k, id)) in servers.iter().zip(&keys) {
                server.reset().await;
                // Each witness also relays an hour-old copy of the other's cosignature.
                let mut cosigs = vec![cosign_at(k, id, base + time::Duration::seconds(tick * 120))];
                for (ok, oid) in keys.iter().filter(|(_, o)| o != id) {
                    cosigs.push(cosign_at(ok, oid, base - time::Duration::hours(1)));
                }
                let dtos: Vec<_> = cosigs
                    .iter()
                    .map(WitnessCosignatureDto::from_witness_cosignature)
                    .collect();
                Mock::given(method("GET"))
                    .and(path("/ledger/sth/5"))
                    .and(query_param("witnesses", "1"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "tree_size": 5, "root_hash": root, "network_id": network_id,
                        "signing_key_id": sth.signing_key_id, "signature": sth.signature,
                        "created_at": created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
                        "cosignatures": dtos,
                    })))
                    .mount(server)
                    .await;
            }
            let mut state = crate::witness_refresh::RefreshState::default();
            let mut extra = DirectoryRefresh {
                witnesses: &[],
                max_per_tick: 0,
                state: &mut state,
                source_url: None,
            };
            refresh_witness_cosignatures(&pool, &chain, &majority, &mut extra, "core", &mut log)
                .await;
            let stored = chain
                .list_witness_cosignatures(&network_id, "core", 5)
                .await
                .unwrap();
            assert_eq!(stored.len(), 2);
            for c in &stored {
                assert_eq!(
                    c.observed_at.unix_timestamp(),
                    (base + time::Duration::seconds(tick * 120)).unix_timestamp(),
                    "tick {tick}: stored copy must be the witness's own current one"
                );
            }
        }
    }

    /// With backfill stalled behind newer observed heads, the cosignatures of the head the node
    /// actually serves (at its mirrored entry count) are refreshed too.
    #[tokio::test]
    #[ignore]
    async fn refresh_covers_the_served_head_when_backfill_lags_the_latest() {
        use avalon_protocol::sth::sign_tree_head;
        use avalon_protocol::witness::sign_witness_cosignature;
        use ed25519_dalek::SigningKey;
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let pool = live_test_pool().await;
        let network_id = format!("avalon-test-served-{}", Uuid::new_v4());
        let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());
        let author = SigningKey::from_bytes(&[9u8; 32]);
        let created_at = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
        let mut prev = avalon_chain::GENESIS_HASH.to_string();
        let mut hashes = Vec::new();
        for seq in 1..=3i64 {
            let mut e = mirror::MirroredEntry {
                source_url: "http://127.0.0.1:1".into(),
                network_id: network_id.clone(),
                shard_id: "core".into(),
                seq,
                event_id: Uuid::new_v4(),
                kind: "test.noop".into(),
                issuer: "i".into(),
                subject: "s".into(),
                payload: Some(serde_json::json!({ "n": seq })),
                event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
                version: 1,
                prev_hash: prev.clone(),
                entry_hash: String::new(),
                batch_id: Uuid::new_v4(),
                verified_tree_size: seq,
            };
            e.entry_hash = e.recomputed_hash().unwrap();
            prev = e.entry_hash.clone();
            hashes.push(e.entry_hash.clone());
            mirror::insert_mirrored_entry(&pool, &e).await.unwrap();
        }
        let served_root = hex::encode(merkle::mth_of_hex_hashes(&hashes).unwrap());
        let heads: Vec<SignedTreeHead> = [(3i64, served_root), (5, hex::encode([2u8; 32]))]
            .iter()
            .map(|(size, root)| sign_tree_head(&author, "op", *size, root, &network_id, created_at))
            .collect();
        // Five other sources report different roots at the served size.
        for i in 0..5u8 {
            let decoy = sign_tree_head(
                &author,
                "op",
                3,
                &hex::encode([0x40 + i; 32]),
                &network_id,
                created_at,
            );
            mirror::insert_observation(
                &pool,
                &ObservedSth::from_sth(
                    format!("http://127.0.0.1:{}", 2 + i),
                    "core",
                    &decoy,
                    created_at,
                ),
            )
            .await
            .unwrap();
        }
        for h in &heads {
            mirror::insert_observation(
                &pool,
                &ObservedSth::from_sth("http://127.0.0.1:1", "core", h, created_at),
            )
            .await
            .unwrap();
        }

        let wk = SigningKey::from_bytes(&[1u8; 32]);
        let wid = hex::encode(wk.verifying_key().to_bytes());
        let server = MockServer::start().await;
        for h in &heads {
            let cosig = sign_witness_cosignature(
                &wk,
                &wid,
                h.tree_size,
                &h.root_hash,
                &network_id,
                created_at,
                OffsetDateTime::now_utc(),
            );
            Mock::given(method("GET"))
                .and(path_regex(format!(r"^/ledger/sth/{}$", h.tree_size)))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tree_size": h.tree_size, "root_hash": h.root_hash, "network_id": network_id,
                    "signing_key_id": h.signing_key_id, "signature": h.signature,
                    "created_at": created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
                    "cosignatures": [WitnessCosignatureDto::from_witness_cosignature(&cosig)],
                })))
                .mount(&server)
                .await;
        }
        let known_list = vec![(wid.clone(), wk.verifying_key())];
        let sources = vec![cosign_gather::WitnessSource {
            key_id: wid.clone(),
            base_url: server.uri(),
        }];
        let majority = MajorityContext {
            known_list: &known_list,
            sources: &sources,
            policy: crate::outbound_policy::OutboundPolicy::new(true),
            self_certifying: &crate::self_certifying_keys::MirrorBounds::default(),
        };
        let mut state = crate::witness_refresh::RefreshState::default();
        let mut extra = DirectoryRefresh {
            witnesses: &[],
            max_per_tick: 0,
            state: &mut state,
            source_url: None,
        };
        let mut log = GatherLog::new();
        for _ in 0..2 {
            refresh_witness_cosignatures(&pool, &chain, &majority, &mut extra, "core", &mut log)
                .await;
        }
        let requests = server.received_requests().await.unwrap();
        let asked = |p: &str| requests.iter().filter(|r| r.url.path() == p).count();
        // Only the observation matching the served root is asked, and not again while fresh.
        assert_eq!(asked("/ledger/sth/3"), 1);
        assert_eq!(asked("/ledger/sth/5"), 2);
        for size in [3i64, 5] {
            let stored = chain
                .list_witness_cosignatures(&network_id, "core", size)
                .await
                .unwrap();
            assert_eq!(stored.len(), 1, "size {size}");
        }
    }

    /// Inserts a hash-chained run of mirrored entries for `network`/core from `source`, returning
    /// the Merkle root over them.
    async fn insert_chain(
        pool: &PgPool,
        network: &str,
        shard: &str,
        source: &str,
        salt: &str,
        n: i64,
    ) -> String {
        let mut prev = avalon_chain::GENESIS_HASH.to_string();
        let mut hashes = Vec::new();
        for seq in 1..=n {
            let mut e = mirror::MirroredEntry {
                source_url: source.into(),
                network_id: network.into(),
                shard_id: shard.into(),
                seq,
                event_id: Uuid::new_v4(),
                kind: "test.noop".into(),
                issuer: "i".into(),
                subject: "s".into(),
                payload: Some(serde_json::json!({ "n": seq, "salt": salt })),
                event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
                version: 1,
                prev_hash: prev.clone(),
                entry_hash: String::new(),
                batch_id: Uuid::new_v4(),
                verified_tree_size: seq,
            };
            e.entry_hash = e.recomputed_hash().unwrap();
            prev = e.entry_hash.clone();
            hashes.push(e.entry_hash.clone());
            mirror::insert_mirrored_entry(pool, &e).await.unwrap();
        }
        hex::encode(merkle::mth_of_hex_hashes(&hashes).unwrap())
    }

    async fn observe(
        pool: &PgPool,
        network: &str,
        shard: &str,
        source: &str,
        size: i64,
        root: &str,
    ) -> ObservedSth {
        use avalon_protocol::sth::sign_tree_head;
        let at = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
        let author = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let sth = sign_tree_head(&author, "op", size, root, network, at);
        let obs = ObservedSth::from_sth(source, shard, &sth, at);
        mirror::insert_observation(pool, &obs).await.unwrap();
        obs
    }

    /// The cached served root must not outlive the rows it was computed from: entries discarded
    /// and re-mirrored to the same count with different content give a different served head.
    #[tokio::test]
    #[ignore]
    async fn the_served_root_cache_is_dropped_when_rows_are_replaced() {
        let pool = live_test_pool().await;
        let net = format!("avalon-test-cache-{}", Uuid::new_v4());
        let shard = format!("game:cache-{}", Uuid::new_v4().simple());
        let src = "http://127.0.0.1:1";
        let latest = observe(&pool, &net, &shard, src, 5, &hex::encode([2u8; 32])).await;
        let mut state = crate::witness_refresh::RefreshState::default();

        let root_a = insert_chain(&pool, &net, &shard, src, "a", 3).await;
        observe(&pool, &net, &shard, src, 3, &root_a).await;
        let got = served_head_observation(&pool, &mut state, &shard, None, &latest).await;
        assert_eq!(got.map(|o| o.root_hash), Some(root_a));

        mirror::discard_mirrored_entries_from(&pool, &net, &shard, 0)
            .await
            .unwrap();
        assert!(
            served_head_observation(&pool, &mut state, &shard, None, &latest)
                .await
                .is_none()
        );

        let root_b = insert_chain(&pool, &net, &shard, src, "b", 3).await;
        observe(&pool, &net, &shard, "http://127.0.0.1:3", 3, &root_b).await;
        let got = served_head_observation(&pool, &mut state, &shard, None, &latest).await;
        assert_eq!(got.map(|o| o.root_hash), Some(root_b));
    }

    /// With one configured source the served head is looked up scoped to it, as the serve path does.
    #[tokio::test]
    #[ignore]
    async fn the_served_head_lookup_is_scoped_to_the_configured_source() {
        let pool = live_test_pool().await;
        let net = format!("avalon-test-scope-{}", Uuid::new_v4());
        let shard = format!("game:scope-{}", Uuid::new_v4().simple());
        let (src, other) = ("http://127.0.0.1:1", "http://127.0.0.1:2");
        let latest = observe(&pool, &net, &shard, src, 5, &hex::encode([2u8; 32])).await;
        let root = insert_chain(&pool, &net, &shard, src, "a", 3).await;
        // Only a different source reported the matching head.
        observe(&pool, &net, &shard, other, 3, &root).await;
        let mut state = crate::witness_refresh::RefreshState::default();
        assert!(
            served_head_observation(&pool, &mut state, &shard, Some(src), &latest)
                .await
                .is_none()
        );
        assert!(
            served_head_observation(&pool, &mut state, &shard, None, &latest)
                .await
                .is_some()
        );
    }

    #[test]
    fn a_single_source_is_the_only_distinct_url_configured_for_the_shard() {
        let p = |s: &str, u: &str| (s.to_string(), u.to_string());
        let one = [
            p("core", "http://a"),
            p("core", "http://a"),
            p("g", "http://b"),
        ];
        assert_eq!(single_source(&one, "core"), Some("http://a"));
        let two = [p("core", "http://a"), p("core", "http://b")];
        assert_eq!(single_source(&two, "core"), None);
        assert_eq!(single_source(&two, "none"), None);
    }

    /// Directory witnesses outside the known list, end to end against a database and mock
    /// witness nodes: selection, gather, store, per-shard backoff, the held-skip and rollback
    /// guard, the observed_at bound, and the row cap counted against real rows.
    #[tokio::test]
    #[ignore]
    async fn refresh_covers_directory_witnesses_within_bounds() {
        use crate::witness_refresh::{DirectoryWitness, RefreshState};
        use avalon_protocol::sth::sign_tree_head;
        use avalon_protocol::witness::sign_witness_cosignature;
        use ed25519_dalek::SigningKey;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        struct Wit {
            sk: SigningKey,
            id: String,
            server: MockServer,
        }
        async fn wit(seed: u8) -> Wit {
            let sk = SigningKey::from_bytes(&[seed; 32]);
            let id = hex::encode(sk.verifying_key().to_bytes());
            Wit {
                sk,
                id,
                server: MockServer::start().await,
            }
        }
        // Serves `observed_at` cosignatures (or none) for one shard's head.
        async fn serve(
            w: &Wit,
            sth: &SignedTreeHead,
            shard: &str,
            observed_at: Option<OffsetDateTime>,
        ) {
            let cosigs: Vec<_> = observed_at
                .map(|at| {
                    let c = sign_witness_cosignature(
                        &w.sk,
                        &w.id,
                        sth.tree_size,
                        &sth.root_hash,
                        &sth.network_id,
                        sth.created_at,
                        at,
                    );
                    WitnessCosignatureDto::from_witness_cosignature(&c)
                })
                .into_iter()
                .collect();
            let fmt = |t: OffsetDateTime| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .unwrap()
            };
            Mock::given(method("GET"))
                .and(path(format!("/ledger/sth/{}", sth.tree_size)))
                .and(query_param("shard_id", shard))
                .and(query_param("witnesses", "1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tree_size": sth.tree_size, "root_hash": sth.root_hash,
                    "network_id": sth.network_id, "signing_key_id": sth.signing_key_id,
                    "signature": sth.signature, "created_at": fmt(sth.created_at),
                    "cosignatures": cosigs,
                })))
                .mount(&w.server)
                .await;
        }
        fn dir(w: &Wit) -> DirectoryWitness {
            DirectoryWitness {
                key_id: w.id.clone(),
                key: w.sk.verifying_key(),
                base_url: w.server.uri(),
                announced_at: OffsetDateTime::now_utc(),
            }
        }

        let pool = live_test_pool().await;
        let network_id = format!("avalon-test-directory-{}", Uuid::new_v4());
        let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());
        let author = SigningKey::from_bytes(&[9u8; 32]);
        let created_at = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
        let suffix = Uuid::new_v4();
        let (shard_a, shard_b, shard_c) = (
            format!("game:dir-a-{suffix}"),
            format!("game:dir-b-{suffix}"),
            format!("game:dir-c-{suffix}"),
        );
        let mut heads = std::collections::HashMap::new();
        for (i, shard) in [&shard_a, &shard_b, &shard_c].into_iter().enumerate() {
            let root = hex::encode([(i + 1) as u8; 32]);
            let sth = sign_tree_head(&author, "op", 5, &root, &network_id, created_at);
            mirror::insert_observation(
                &pool,
                &ObservedSth::from_sth("http://127.0.0.1:1", shard, &sth, created_at),
            )
            .await
            .unwrap();
            heads.insert(shard.clone(), sth);
        }

        // d1 cosigns shards a and b, d2 only b, d3 puts a far-future timestamp on b.
        let (d1, d2, d3) = (wit(1).await, wit(2).await, wit(3).await);
        let now = OffsetDateTime::now_utc();
        let cur = now - time::Duration::seconds(30);
        serve(&d1, &heads[&shard_a], &shard_a, Some(cur)).await;
        serve(&d1, &heads[&shard_b], &shard_b, Some(cur)).await;
        serve(&d2, &heads[&shard_a], &shard_a, None).await;
        serve(&d2, &heads[&shard_b], &shard_b, Some(cur)).await;
        serve(
            &d3,
            &heads[&shard_b],
            &shard_b,
            Some(now + time::Duration::days(1)),
        )
        .await;
        serve(&d3, &heads[&shard_a], &shard_a, None).await;

        let directory = vec![dir(&d1), dir(&d2), dir(&d3)];
        let majority = MajorityContext {
            known_list: &[],
            sources: &[],
            policy: crate::outbound_policy::OutboundPolicy::new(true),
            self_certifying: &crate::self_certifying_keys::MirrorBounds::default(),
        };
        let mut state = RefreshState::default();
        let mut log = GatherLog::new();
        let stored_ids = |shard: String| {
            let chain = &chain;
            let network_id = network_id.clone();
            async move {
                let mut ids: Vec<String> = chain
                    .list_witness_cosignatures(&network_id, &shard, 5)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|c| c.witness_key_id)
                    .collect();
                ids.sort();
                ids
            }
        };
        for shard in [&shard_a, &shard_b] {
            let mut extra = DirectoryRefresh {
                witnesses: &directory,
                max_per_tick: 3,
                state: &mut state,
                source_url: None,
            };
            refresh_witness_cosignatures(&pool, &chain, &majority, &mut extra, shard, &mut log)
                .await;
        }
        assert_eq!(stored_ids(shard_a.clone()).await, vec![d1.id.clone()]);
        let mut both = vec![d1.id.clone(), d2.id.clone()];
        both.sort();
        assert_eq!(
            stored_ids(shard_b.clone()).await,
            both,
            "d2 lacking shard a must not be skipped on shard b; d3's future timestamp is not stored"
        );

        // Held witness: an older observation never replaces the stored one, a newer one does.
        let observed = |id: &str, shard: &str| {
            let (chain, network_id, id, shard) = (
                chain.clone(),
                network_id.clone(),
                id.to_string(),
                shard.to_string(),
            );
            async move {
                chain
                    .list_witness_cosignatures(&network_id, &shard, 5)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|c| c.witness_key_id == id)
                    .unwrap()
                    .observed_at
            }
        };
        let first = observed(&d1.id, &shard_a).await;
        d1.server.reset().await;
        serve(
            &d1,
            &heads[&shard_a],
            &shard_a,
            Some(cur - time::Duration::seconds(20)),
        )
        .await;
        let mut extra = DirectoryRefresh {
            witnesses: &directory,
            max_per_tick: 3,
            state: &mut state,
            source_url: None,
        };
        refresh_witness_cosignatures(&pool, &chain, &majority, &mut extra, &shard_a, &mut log)
            .await;
        assert_eq!(observed(&d1.id, &shard_a).await, first, "no rollback");
        d1.server.reset().await;
        serve(
            &d1,
            &heads[&shard_a],
            &shard_a,
            Some(cur + time::Duration::seconds(20)),
        )
        .await;
        let mut extra = DirectoryRefresh {
            witnesses: &directory,
            max_per_tick: 3,
            state: &mut state,
            source_url: None,
        };
        refresh_witness_cosignatures(&pool, &chain, &majority, &mut extra, &shard_a, &mut log)
            .await;
        assert!(
            observed(&d1.id, &shard_a).await > first,
            "a newer observation refreshes"
        );

        // Row cap: with max_per_tick 2 the cap is 4 rows from outside the list; fill it with
        // real rows and a new directory witness is not even asked.
        let fillers: Vec<Wit> = vec![wit(10).await, wit(11).await, wit(12).await, wit(13).await];
        for f in &fillers {
            let c = sign_witness_cosignature(
                &f.sk,
                &f.id,
                5,
                &heads[&shard_c].root_hash,
                &network_id,
                created_at,
                cur,
            );
            chain.store_witness_cosignature(&shard_c, &c).await.unwrap();
        }
        let d4 = wit(4).await;
        serve(&d4, &heads[&shard_c], &shard_c, Some(cur)).await;
        let directory = vec![dir(&d4)];
        let mut extra = DirectoryRefresh {
            witnesses: &directory,
            max_per_tick: 2,
            state: &mut state,
            source_url: None,
        };
        refresh_witness_cosignatures(&pool, &chain, &majority, &mut extra, &shard_c, &mut log)
            .await;
        assert_eq!(d4.server.received_requests().await.unwrap().len(), 0);
        assert_eq!(stored_ids(shard_c.clone()).await.len(), 4);
    }

    /// The equivocation gate ([`backfill_network`]'s first check)
    /// against real Postgres: a network with an unresolved finding
    /// must not have anything written to `mirrored_entries`, even when
    /// handed a well-formed, internally-consistent observation set that
    /// would otherwise corroborate cleanly. This is the "mirror stops
    /// trusting/serving the affected key's current head" acceptance
    /// criterion — exercised directly against the real gate function, not a
    /// re-implementation of its logic.
    #[tokio::test]
    #[ignore]
    async fn backfill_network_refuses_to_proceed_while_an_equivocation_is_unresolved() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let client = crate::node_http::NodeClient::new();
        let network_id = format!("avalon-test-gate-{}", Uuid::new_v4());

        mirror::record_equivocation(
            &pool,
            &mirror::EquivocationFinding {
                network_id: network_id.clone(),
                shard_id: mirror::CORE_SHARD_ID.to_string(),
                tree_size: 10,
                source_a: "peer-a".to_string(),
                root_hash_a: "aa".repeat(32),
                source_b: "peer-b".to_string(),
                root_hash_b: "bb".repeat(32),
                resolved_at: None,
                resolved_root_hash: None,
            },
        )
        .await
        .expect("record_equivocation failed");

        // A single, internally-consistent observation — if the gate were
        // not checked first, this would corroborate trivially (one peer
        // "agreeing" with itself) and backfill would proceed.
        let observations = vec![(
            "peer-a".to_string(),
            SignedTreeHead {
                tree_size: 10,
                root_hash: "aa".repeat(32),
                network_id: network_id.clone(),
                signing_key_id: "test-key".to_string(),
                signature: "sig".to_string(),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )];

        backfill_network(
            &client,
            &pool,
            &indexer,
            &network_id,
            mirror::CORE_SHARD_ID,
            &observations,
            None,
            0,
        )
        .await
        .expect("backfill_network should return Ok(()) rather than error when gated");

        let progress = mirror::mirrored_progress(&pool, &network_id, mirror::CORE_SHARD_ID, None)
            .await
            .expect("mirrored_progress failed");
        assert_eq!(
            progress.verified_count, 0,
            "backfill must not write anything while the network has an unresolved equivocation"
        );
    }

    /// Once the finding is resolved, the gate clears and `backfill_network`
    /// proceeds again (calling into real backfill logic, which will attempt
    /// real HTTP requests to the peer URL and fail gracefully — the point
    /// here is only that it gets *past* the gate, not that it completes a
    /// backfill against an unreachable peer).
    #[tokio::test]
    #[ignore]
    async fn backfill_network_proceeds_again_once_the_finding_is_resolved() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let client = crate::node_http::NodeClient::new();
        let network_id = format!("avalon-test-gate-cleared-{}", Uuid::new_v4());

        mirror::record_equivocation(
            &pool,
            &mirror::EquivocationFinding {
                network_id: network_id.clone(),
                shard_id: mirror::CORE_SHARD_ID.to_string(),
                tree_size: 10,
                source_a: "peer-a".to_string(),
                root_hash_a: "aa".repeat(32),
                source_b: "peer-b".to_string(),
                root_hash_b: "bb".repeat(32),
                resolved_at: None,
                resolved_root_hash: None,
            },
        )
        .await
        .expect("record_equivocation failed");

        mirror::resolve_equivocation(
            &pool,
            &network_id,
            mirror::CORE_SHARD_ID,
            10,
            &"aa".repeat(32),
        )
        .await
        .expect("resolve_equivocation failed");

        let unresolved =
            mirror::unresolved_equivocations(&pool, &network_id, mirror::CORE_SHARD_ID)
                .await
                .expect("unresolved_equivocations failed");
        assert!(
            unresolved.is_empty(),
            "gate should be clear after resolution"
        );

        // With no observations at all, backfill_network takes its "no
        // agreement" early return — still proves it got past the
        // equivocation gate (which would otherwise have returned first with
        // a distinct log line) without needing a reachable peer.
        let observations: Vec<(String, SignedTreeHead)> = Vec::new();
        let result = backfill_network(
            &client,
            &pool,
            &indexer,
            &network_id,
            mirror::CORE_SHARD_ID,
            &observations,
            None,
            0,
        )
        .await;
        assert!(result.is_ok());
    }

    fn mk_entry(
        network_id: &str,
        seq: i64,
        kind: &str,
        identity_id: IdentityId,
        payload: Option<serde_json::Value>,
    ) -> mirror::MirroredEntry {
        let who = format!(
            "identity:{identity_id}:self:{}",
            kind.trim_start_matches("identity.")
        );
        let mut entry = mirror::MirroredEntry {
            source_url: "http://peer.invalid".to_string(),
            network_id: network_id.to_string(),
            shard_id: mirror::CORE_SHARD_ID.to_string(),
            seq,
            event_id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: who.clone(),
            subject: who,
            payload,
            event_timestamp: OffsetDateTime::now_utc(),
            version: if matches!(kind, "identity.created" | "identity.signing_key_added") {
                2
            } else {
                1
            },
            prev_hash: "00".repeat(32),
            entry_hash: format!("{seq:064x}"),
            batch_id: Uuid::new_v4(),
            verified_tree_size: seq,
        };
        // A pruned (payload-less) fixture keeps its placeholder hash: it can never be stored.
        if let Some(hash) = entry.recomputed_hash() {
            entry.entry_hash = hash;
        }
        entry
    }

    fn b64(bytes: &[u8]) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
    }

    fn created_payload(who: &TestIdentity, name: &str) -> serde_json::Value {
        serde_json::to_value(who.created_payload(name)).unwrap()
    }

    fn passkey_payload(identity_id: IdentityId, passkey_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "passkey_id": passkey_id,
            "identity_id": identity_id,
            "credential_id": b64(Uuid::new_v4().as_bytes()),
            "passkey_data": {"k": 1},
            "label": null,
        })
    }

    /// Entries as a core shard records a fresh registration: `identity.created`
    /// first, then the passkey and the inception signing key.
    fn full_history(network_id: &str, identities: usize) -> Vec<mirror::MirroredEntry> {
        let mut entries = Vec::new();
        let mut seq = 0;
        for _ in 0..identities {
            let who = TestIdentity::new();
            let id = who.id;
            let key = serde_json::json!({
                "signing_key_id": Uuid::new_v4(),
                "public_key": b64(&who.public_key()),
                "device_label": null,
                "approved_by_signing_key_id": Uuid::new_v4(),
                "identity_id": id,
                "kind": "inception",
            });
            for (kind, payload) in [
                (
                    "identity.created",
                    created_payload(&who, &format!("replay-{id}")),
                ),
                (
                    "identity.passkey_registered",
                    passkey_payload(id, Uuid::new_v4()),
                ),
                ("identity.signing_key_added", key),
            ] {
                seq += 1;
                entries.push(mk_entry(network_id, seq, kind, id, Some(payload)));
            }
        }
        entries
    }

    async fn claimed(pool: &PgPool, entries: &[&mirror::MirroredEntry]) -> i64 {
        let ids: Vec<Uuid> = entries.iter().map(|e| e.event_id).collect();
        sqlx::query_scalar("SELECT count(*) FROM indexer_applied_events WHERE event_id = ANY($1)")
            .bind(&ids)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn projected_counts(pool: &PgPool, entries: &[mirror::MirroredEntry]) -> (i64, i64, i64) {
        let all: Vec<&mirror::MirroredEntry> = entries.iter().collect();
        let ident_ids: Vec<IdentityId> = entries
            .iter()
            .filter(|e| e.kind == "identity.created")
            .map(|e| {
                IdentityId::parse(e.payload.as_ref().unwrap()["identity_id"].as_str().unwrap())
                    .unwrap()
            })
            .collect();
        let passkeys: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM indexer_identity_passkeys WHERE identity_id = ANY($1)",
        )
        .bind(&ident_ids)
        .fetch_one(pool)
        .await
        .unwrap();
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM indexer_identity_signing_keys WHERE identity_id = ANY($1)",
        )
        .bind(&ident_ids)
        .fetch_one(pool)
        .await
        .unwrap();
        (claimed(pool, &all).await, passkeys, keys)
    }

    async fn store_entries(pool: &PgPool, entries: &[mirror::MirroredEntry]) {
        for entry in entries {
            mirror::insert_mirrored_entry(pool, entry).await.unwrap();
        }
    }

    /// Writes a row the way a pre-#1165 node could have stored a pruned entry, bypassing the
    /// storage-boundary check, to exercise handling of legacy rows.
    async fn insert_legacy_pruned_row(pool: &PgPool, e: &mirror::MirroredEntry) {
        sqlx::query(
            "INSERT INTO mirrored_entries (source_url, network_id, shard_id, seq, event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id, verified_tree_size) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13, $14)",
        )
        .bind(&e.source_url)
        .bind(&e.network_id)
        .bind(&e.shard_id)
        .bind(e.seq)
        .bind(e.event_id)
        .bind(&e.kind)
        .bind(&e.issuer)
        .bind(&e.subject)
        .bind(e.event_timestamp)
        .bind(e.version)
        .bind(&e.prev_hash)
        .bind(&e.entry_hash)
        .bind(e.batch_id)
        .bind(e.verified_tree_size)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn stored_count(pool: &PgPool, network_id: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM mirrored_entries WHERE network_id = $1")
            .bind(network_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn fresh_network(tag: &str) -> String {
        format!("avalon-test-{tag}-{}", Uuid::new_v4())
    }

    /// Replaying child-before-parent history into an empty replica projects every
    /// entry on first delivery.
    #[tokio::test]
    #[ignore]
    async fn replaying_children_ahead_of_identity_created_projects_completely() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let entries = full_history(&fresh_network("replay"), 3);
        let mut blocked = false;
        for entry in &entries {
            store_and_project(&pool, &indexer, entry, &mut blocked)
                .await
                .unwrap();
        }
        assert!(!blocked);
        assert_eq!(projected_counts(&pool, &entries).await, (9, 3, 3));
    }

    /// Entries stored without a successful projection are re-driven, once.
    #[tokio::test]
    #[ignore]
    async fn unapplied_mirrored_entries_are_reprojected() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("reproject");
        let entries = full_history(&network_id, 2);
        store_entries(&pool, &entries).await;
        assert_eq!(projected_counts(&pool, &entries).await, (0, 0, 0));

        let report = reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        assert_eq!(report.projected, 6);
        assert_eq!(projected_counts(&pool, &entries).await, (6, 2, 2));

        let again = reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        assert_eq!(again, ReprojectReport::default());
    }

    /// Two registrations sharing a display name (the second fails permanently),
    /// followed by a full child-first history that must still project.
    fn poison_history(network_id: &str) -> Vec<mirror::MirroredEntry> {
        let name = format!("poison-{}", Uuid::new_v4());
        let (a, b) = (TestIdentity::new(), TestIdentity::new());
        let mut entries = vec![
            mk_entry(
                network_id,
                1,
                "identity.created",
                a.id,
                Some(created_payload(&a, &name)),
            ),
            mk_entry(
                network_id,
                2,
                "identity.created",
                b.id,
                Some(created_payload(&b, &name)),
            ),
        ];
        for mut e in full_history(network_id, 1) {
            e.seq += 2;
            entries.push(e);
        }
        entries
    }

    /// A permanently failing entry is parked and the entries behind it still
    /// project; a later forced pass skips it instead of retrying.
    #[tokio::test]
    #[ignore]
    async fn a_permanently_failing_entry_is_parked_without_blocking_later_entries() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("poison");
        let entries = poison_history(&network_id);
        store_entries(&pool, &entries).await;

        let report = reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        assert_eq!(
            (report.projected, report.parked, report.blocked),
            (4, 1, false)
        );
        assert!(is_parked(&entries[1].event_id));

        mark_projection_failed(&network_id, mirror::CORE_SHARD_ID);
        let again = reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        assert_eq!(
            again,
            ReprojectReport::default(),
            "parked entry must be skipped"
        );
    }

    /// A restart empties the parked set, so previously parked entries are retried.
    #[tokio::test]
    #[ignore]
    async fn parked_entries_are_retried_after_a_restart() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("restart");
        let entries = poison_history(&network_id);
        store_entries(&pool, &entries).await;
        reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();

        PARKED.lock().unwrap().remove(&entries[1].event_id);
        mark_projection_failed(&network_id, mirror::CORE_SHARD_ID);
        let after_restart =
            reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
                .await
                .unwrap();
        assert_eq!(after_restart.parked, 1, "the entry must be attempted again");
    }

    /// A pruned-payload entry is never projected or parked, and storing one is refused.
    #[tokio::test]
    #[ignore]
    async fn undecodable_entries_are_skipped_not_parked() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("skipped");
        let pruned = mk_entry(
            &network_id,
            1,
            "identity.created",
            IdentityId::random_for_tests(),
            None,
        );

        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            project_mirrored_entry(&mut tx, &indexer, &pruned)
                .await
                .unwrap(),
            ProjectionOutcome::Skipped
        );
        tx.rollback().await.unwrap();

        // A pruned entry is refused at the storage boundary, so it is neither stored nor parked.
        let mut blocked = false;
        let refused = store_and_project(&pool, &indexer, &pruned, &mut blocked).await;
        assert!(matches!(
            refused,
            Err(MirrorWatcherError::Storage(
                avalon_chain::SettlementError::MirroredPayloadUnverifiable { seq: 1 }
            ))
        ));
        assert_eq!(stored_count(&pool, &network_id).await, 0);
        assert!(!is_parked(&pruned.event_id));
    }

    /// A display-name conflict is deterministic: the entry fails permanently,
    /// stays unclaimed (savepoint rolled back) and does not stop the next one.
    #[tokio::test]
    #[ignore]
    async fn a_display_name_conflict_fails_permanently_and_leaves_the_entry_unclaimed() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("dupname");
        let name = format!("dup-{}", Uuid::new_v4());
        let (a, b) = (TestIdentity::new(), TestIdentity::new());
        let first = mk_entry(
            &network_id,
            1,
            "identity.created",
            a.id,
            Some(created_payload(&a, &name)),
        );
        let second = mk_entry(
            &network_id,
            2,
            "identity.created",
            b.id,
            Some(created_payload(&b, &name)),
        );
        let after = mk_entry(
            &network_id,
            3,
            "identity.passkey_registered",
            a.id,
            Some(passkey_payload(a.id, Uuid::new_v4())),
        );
        for e in [&first, &second, &after] {
            mirror::insert_mirrored_entry(&pool, e).await.unwrap();
        }

        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            project_mirrored_entry(&mut tx, &indexer, &first)
                .await
                .unwrap(),
            ProjectionOutcome::Applied
        );
        let outcome = project_mirrored_entry(&mut tx, &indexer, &second)
            .await
            .unwrap();
        assert!(
            matches!(outcome, ProjectionOutcome::Permanent(_)),
            "{outcome:?}"
        );
        tx.commit().await.unwrap();
        assert_eq!(claimed(&pool, &[&first]).await, 1);
        assert_eq!(claimed(&pool, &[&second]).await, 0);

        let report = reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        assert_eq!(
            (report.projected, report.parked, report.blocked),
            (1, 1, false)
        );
        assert_eq!(claimed(&pool, &[&after]).await, 1);
    }

    /// A transient failure stops the pass at that entry and keeps the scan due.
    #[tokio::test]
    #[ignore]
    async fn a_transient_failure_stops_the_pass_and_reports_blocked() {
        let pool = live_test_pool().await;
        let network_id = fresh_network("transient");
        store_entries(&pool, &full_history(&network_id, 1)).await;

        let mut calls = 0;
        let report = reproject_with(&pool, &network_id, mirror::CORE_SHARD_ID, |_| {
            calls += 1;
            let outcome = if calls == 2 {
                ProjectionOutcome::Transient("db down".to_string())
            } else {
                ProjectionOutcome::Applied
            };
            async move { Ok(outcome) }
        })
        .await
        .unwrap();
        assert_eq!(
            calls, 2,
            "entries after the transient failure must not be attempted"
        );
        assert_eq!((report.projected, report.blocked), (1, true));
        assert!(scan_due(&network_id, mirror::CORE_SHARD_ID));
    }

    /// Self-certifying shards are stored but never projected.
    #[tokio::test]
    #[ignore]
    async fn reproject_skips_self_certifying_shards() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("selfcert");
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let shard =
            avalon_protocol::shard_identity::derive_self_certifying_id(&key.verifying_key());
        let mut entries = full_history(&network_id, 1);
        for e in &mut entries {
            e.shard_id = shard.clone();
        }
        store_entries(&pool, &entries).await;
        let report = reproject_unapplied(&pool, &indexer, &network_id, &shard)
            .await
            .unwrap();
        assert_eq!(report, ReprojectReport::default());
        assert_eq!(claimed(&pool, &entries.iter().collect::<Vec<_>>()).await, 0);
    }

    /// Pruned-payload entries are neither projected nor treated as failures.
    #[tokio::test]
    #[ignore]
    async fn reproject_skips_pruned_payload_entries() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("pruned");
        let pruned = mk_entry(
            &network_id,
            1,
            "identity.created",
            IdentityId::random_for_tests(),
            None,
        );
        insert_legacy_pruned_row(&pool, &pruned).await;
        let report = reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        assert_eq!(report, ReprojectReport::default());
        assert_eq!(claimed(&pool, &[&pruned]).await, 0);
    }

    /// While projection is blocked, new entries are stored but left unapplied,
    /// so a revoke can never apply ahead of the registration it follows.
    #[tokio::test]
    #[ignore]
    async fn a_blocked_projection_keeps_later_entries_unapplied_so_a_revoke_stays_ordered() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let network_id = fresh_network("revoke");
        let (who, passkey_id) = (TestIdentity::new(), Uuid::new_v4());
        let id = who.id;
        let created = mk_entry(
            &network_id,
            1,
            "identity.created",
            id,
            Some(created_payload(&who, &format!("revoke-{id}"))),
        );
        let registered = mk_entry(
            &network_id,
            2,
            "identity.passkey_registered",
            id,
            Some(passkey_payload(id, passkey_id)),
        );
        let revoked = mk_entry(
            &network_id,
            3,
            "identity.passkey_revoked",
            id,
            Some(serde_json::json!({ "passkey_id": passkey_id, "identity_id": id })),
        );
        let mut blocked = true;
        for e in [&created, &registered, &revoked] {
            store_and_project(&pool, &indexer, e, &mut blocked)
                .await
                .unwrap();
        }
        assert_eq!(claimed(&pool, &[&created, &registered, &revoked]).await, 0);

        reproject_unapplied(&pool, &indexer, &network_id, mirror::CORE_SHARD_ID)
            .await
            .unwrap();
        let revoked_at: Option<OffsetDateTime> = sqlx::query_scalar(
            "SELECT revoked_at FROM indexer_identity_passkeys WHERE passkey_id = $1",
        )
        .bind(passkey_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(revoked_at.is_some(), "the passkey must end up revoked");
    }

    fn created_event(who: &TestIdentity, embedded: IdentityId) -> ProtocolEvent {
        let global = GlobalId::new("identity", &embedded.to_string(), "self", "created");
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: global.clone(),
            subject: global,
            payload: created_payload(who, "target-test"),
            timestamp: OffsetDateTime::now_utc(),
            version: 2,
            identity_chain: None,
        }
    }

    #[test]
    fn identity_row_target_requires_a_v2_creation_with_a_matching_embedded_id() {
        let who = TestIdentity::new();
        assert_eq!(
            identity_row_target(&created_event(&who, who.id)),
            Some((who.id, who.public_key()))
        );
        assert_eq!(
            identity_row_target(&created_event(&who, IdentityId::random_for_tests())),
            None
        );
        let mut v1 = created_event(&who, who.id);
        v1.version = 1;
        assert_eq!(identity_row_target(&v1), None);
        let mut other_kind = created_event(&who, who.id);
        other_kind.kind = "identity.passkey_registered".to_string();
        assert_eq!(identity_row_target(&other_kind), None);
        let mut wrong_key = created_event(&who, who.id);
        wrong_key.payload["identity_id"] =
            serde_json::json!(IdentityId::random_for_tests().to_string());
        assert_eq!(identity_row_target(&wrong_key), None);
    }

    #[tokio::test]
    #[ignore]
    async fn ensure_identity_row_only_inserts_for_a_valid_creation() {
        let pool = live_test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let (good, bad) = (TestIdentity::new(), TestIdentity::new());
        let mut event = created_event(&good, good.id);
        event.timestamp = OffsetDateTime::UNIX_EPOCH + time::Duration::days(400);
        ensure_identity_row_exists(&mut tx, &event).await.unwrap();
        ensure_identity_row_exists(&mut tx, &created_event(&bad, good.id))
            .await
            .unwrap();
        let rows: Vec<(IdentityId, OffsetDateTime)> =
            sqlx::query_as("SELECT id, created_at FROM identities WHERE id = ANY($1)")
                .bind(vec![good.id, bad.id])
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(rows, vec![(good.id, event.timestamp)]);
    }

    #[test]
    fn the_scan_flag_is_cleared_by_a_failure_and_set_by_a_clean_pass() {
        let net = fresh_network("flag");
        assert!(scan_due(&net, "core"), "first pass after start scans");
        mark_scan_clean(&net, "core");
        assert!(!scan_due(&net, "core"));
        assert!(scan_due(&net, "other"), "flags are per shard");
        mark_projection_failed(&net, "core");
        assert!(scan_due(&net, "core"));
    }

    #[test]
    fn parking_is_bounded() {
        let mut set = std::collections::HashSet::new();
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let results: Vec<bool> = ids.iter().map(|id| park_in(&mut set, *id, 3)).collect();
        assert_eq!(results, vec![true, true, true, false]);
        assert!(
            park_in(&mut set, ids[0], 3),
            "an already parked entry stays parked"
        );
    }

    fn dummy_sth(network_id: &str) -> Vec<(String, SignedTreeHead)> {
        vec![(
            "peer-a".to_string(),
            SignedTreeHead {
                tree_size: 10,
                root_hash: "aa".repeat(32),
                network_id: network_id.to_string(),
                signing_key_id: "test-key".to_string(),
                signature: "sig".to_string(),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )]
    }

    /// `backfill_network` re-projects stored entries, but only once the
    /// equivocation gate has passed.
    #[tokio::test]
    #[ignore]
    async fn backfill_network_reprojects_only_after_the_equivocation_gate() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let client = crate::node_http::NodeClient::new();
        let network_id = fresh_network("wiring");
        let entries = full_history(&network_id, 1);
        store_entries(&pool, &entries).await;
        let refs: Vec<&mirror::MirroredEntry> = entries.iter().collect();

        mirror::record_equivocation(
            &pool,
            &mirror::EquivocationFinding {
                network_id: network_id.clone(),
                shard_id: mirror::CORE_SHARD_ID.to_string(),
                tree_size: 10,
                source_a: "peer-a".to_string(),
                root_hash_a: "aa".repeat(32),
                source_b: "peer-b".to_string(),
                root_hash_b: "bb".repeat(32),
                resolved_at: None,
                resolved_root_hash: None,
            },
        )
        .await
        .unwrap();
        let obs = dummy_sth(&network_id);
        backfill_network(
            &client,
            &pool,
            &indexer,
            &network_id,
            mirror::CORE_SHARD_ID,
            &obs,
            None,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            claimed(&pool, &refs).await,
            0,
            "gated: nothing may be projected"
        );

        mirror::resolve_equivocation(
            &pool,
            &network_id,
            mirror::CORE_SHARD_ID,
            10,
            &"aa".repeat(32),
        )
        .await
        .unwrap();
        backfill_network(
            &client,
            &pool,
            &indexer,
            &network_id,
            mirror::CORE_SHARD_ID,
            &[],
            None,
            0,
        )
        .await
        .unwrap();
        assert_eq!(claimed(&pool, &refs).await, 3);
    }
}
