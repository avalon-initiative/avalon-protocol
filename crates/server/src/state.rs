use std::sync::Arc;

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PostgresIndexer;
use sqlx::PgPool;
use webauthn_rs::prelude::Webauthn;

use crate::chat::ChatBus;
use crate::nodes::{PeerTable, ShardRegistry};
use crate::presence::PresenceStore;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub chain: PostgresSettlementProvider,
    /// The query/index layer (issue #42) — `handlers::register_finish` and
    /// `update_profile` call `indexer.apply_in_tx` in the same transaction
    /// as the identity/outbox rows instead of writing `profiles`
    /// themselves. See `docs/architecture/query-and-indexing.md`.
    pub indexer: PostgresIndexer,
    /// Built once at startup from `AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`.
    /// `Webauthn` itself isn't cheap to reconstruct (origin parsing/validation),
    /// so it's shared behind an `Arc` rather than rebuilt per request.
    pub webauthn: Arc<Webauthn>,
    /// In-process, per-node presence state (issue #16). Never persisted —
    /// see `crate::presence` module docs / ADR #78.
    pub presence: PresenceStore,
    /// Fan-out for guild channel and conversation message push (issue
    /// #438). Never persisted — Postgres is the source of truth, this only
    /// wakes up an already-subscribed client. See `crate::chat`.
    pub chat: ChatBus,
    /// `AVALON_SETTLEMENT_SUBMIT_KEY` (issue #313) — the bearer credential
    /// `POST /ledger/submit` requires. `None` means the endpoint refuses
    /// every request rather than accepting an unauthenticated one; see
    /// `crate::settlement::submit_ledger_batch`.
    pub settlement_submit_key: Option<String>,
    /// This node's local peer table (issue #362) — in-process,
    /// non-durable, same posture `presence` takes. See `crate::nodes`.
    pub peers: PeerTable,
    /// `AVALON_MANAGED_HOSTING_VERIFY_KEY` (issue #531) — the one hosted
    /// integrator's settlement *public* key, used only to verify a
    /// `POST /ledger/finalize-batch` caller's signature; this node never
    /// holds the corresponding private key. `None` means both
    /// `/ledger/prepare-batch` and `/ledger/finalize-batch` refuse every
    /// request — an operator who never turns on managed hosting gets
    /// endpoints that exist but accept nothing, never ones that are
    /// silently open. **Interim, single-key-per-node**: #543's real
    /// per-shard key resolution (via issuer-key registration) isn't built
    /// yet — see `crate::settlement`'s module doc comment.
    pub managed_hosting_verify_key: Option<ed25519_dalek::VerifyingKey>,
    /// `AVALON_KNOWN_SHARDS`/`AVALON_SHARD_VERIFY_KEYS` (issue #529) —
    /// which shards this node aggregates into a cross-shard root, and how
    /// it verifies each one's STH. `None` (the default) falls back to the
    /// one-shard degenerate case — see `crate::cross_shard`'s own module
    /// doc comment.
    pub known_shards: Option<crate::cross_shard::KnownShardsConfig>,
    /// Issue #526: a handle to the outbox worker's own live
    /// remote-submit failure tracker, for `GET /ledger/remote-submit-status`
    /// (`crate::settlement::remote_submit_status`) to read. `None` when
    /// this node has no `AVALON_SETTLEMENT_REMOTE_URL(S)` configured at
    /// all — it isn't forwarding to any remote authority, so there is
    /// nothing to ever report as failing.
    pub remote_submit_status: Option<crate::outbox::RemoteSubmitStatus>,
    /// `AVALON_OWN_SHARD_ID` (issue #573), defaulting to `"core"`. Which
    /// shard this node's own `chain`/`ledger_entries` represents, when it
    /// has any local history — see `crate::settlement`'s module doc
    /// comment for why this matters now that a node can hold local
    /// history for one shard while mirroring a *different* one.
    pub own_shard_id: String,
    /// Issue #573: `AVALON_MIRROR_PEERS` parsed into a `shard_id -> url`
    /// map, for `crate::settlement`'s shard-scoped mirror reads. See
    /// `crate::settlement::ShardMirrorSources`'s own doc comment.
    pub shard_mirror_sources: crate::settlement::ShardMirrorSources,
    /// Issue #583: local guild-channel/conversation subscriber refcounts —
    /// always present (cheap, no config needed) regardless of whether the
    /// DHT itself is enabled, so `crate::chat`'s subscribe handlers have
    /// one code path either way. Only actually reaches the DHT when
    /// `dht_commands` below is `Some`; see `crate::interest`'s own module
    /// doc comment.
    pub interest: crate::interest::InterestRegistry,
    /// Issue #583: `Some` only when `AVALON_DHT_ENABLED` is set — the
    /// handle #584's relay path (and this node's own interest-refresh
    /// worker) send `crate::dht::DhtCommand`s through. `None` means this
    /// node has no DHT identity at all yet (#582's own default-off
    /// posture), so nothing here can look anything up or register
    /// anything either.
    pub dht_commands: Option<crate::dht::DhtCommandSender>,
    /// Issue #584: this node's own `AVALON_NODE_URL`
    /// (`AnnounceConfig::own_base_url`), so `crate::realtime_relay` can
    /// filter its own base URL out of a DHT interest lookup's results —
    /// a node with a local subscriber for the same scope it's relaying
    /// for would otherwise see itself come back from `interest::lookup`
    /// and relay-POST to itself. `None` means this node can't announce
    /// itself anywhere (`AnnounceConfig`'s own degenerate case), so
    /// nothing to filter either.
    pub own_base_url: Option<String>,
    /// Issue #585: the optional per-hoster Redis fast-path in front of
    /// interest lookups — `None` unless `AVALON_REDIS_URL` is set (see
    /// `crate::interest::RedisFastPath`'s own module doc comment). Never
    /// required for correctness; the DHT lookup this sits in front of
    /// stays authoritative either way.
    pub interest_redis_fast_path: Option<crate::interest::RedisFastPath>,
    /// Issue #596: shared with `mirror_watcher::run_worker` (when spawned)
    /// so `POST /mirror/notify`'s handler (`crate::mirror_push::notify`)
    /// can wake its poll loop early instead of waiting out the rest of
    /// the current `AVALON_MIRROR_POLL_INTERVAL_SECS` tick. Always
    /// present, same "cheap, no config needed" posture as `interest`
    /// above — a node not running the mirror-watcher at all still has
    /// somewhere harmless for a stray notification to land.
    pub mirror_wake: std::sync::Arc<tokio::sync::Notify>,
    /// Issue #517: host resource metrics (CPU/memory/disk/process) for
    /// `GET /nodes/status`'s `resources` block — a shared snapshot kept
    /// current by `crate::resources::start_sampler`, always present (no
    /// config needed) and always best-effort. See `crate::resources`'s
    /// own module doc comment.
    pub host_metrics: crate::resources::HostMetricsSampler,
    /// Issue #599, Layer 2: this node's anti-entropy view of every shard it
    /// currently knows exists, gossiped over the same announce mechanism
    /// #362 already runs. Always present (no config needed — the registry
    /// starts empty and grows purely from what peers gossip plus this
    /// node's own authoritative shard, if any). See `crate::nodes::ShardRegistry`'s
    /// own doc comment; consumed by `crate::cross_shard` (aggregation) and
    /// `crate::mirror_watcher` (opt-in auto-mirroring).
    pub shard_registry: ShardRegistry,
    /// `AVALON_ADMIN_TOKEN` (issue #658) — the bearer credential
    /// `GET`/`POST /nodes/log-level` require. Deliberately a *separate*
    /// secret from `settlement_submit_key` above — see `crate::admin`'s
    /// own module doc comment for why. `None` means those endpoints refuse
    /// every request, same posture `settlement_submit_key` already
    /// establishes for its own endpoints.
    pub admin_token: Option<String>,
    /// Issue #658: the live handle to this process's own `tracing`
    /// env-filter, built once in `main::init_tracing` — swapping through
    /// this changes what gets logged on the very next call, no restart.
    pub log_reload_handle: crate::admin::LogReloadHandle,
    /// Issue #629: this node's own record of which distinct peers have
    /// confirmed mirroring which shard, kept current by
    /// `crate::replication::run_worker`. Always present (no config
    /// needed, same posture `shard_registry` above already takes) —
    /// starts empty and fills in over the first few poll ticks.
    pub mirror_confirmations: crate::replication::MirrorConfirmationRegistry,
    /// Issue #629: this node's resolved minimum-replication gate
    /// configuration (`AVALON_MIN_MIRROR_CONFIRMATIONS`/
    /// `AVALON_MIRROR_GRACE_PERIOD_HOURS`), read once at startup. See
    /// `crate::replication`'s own module doc comment.
    pub replication_gate: crate::replication::ReplicationGateConfig,
}
