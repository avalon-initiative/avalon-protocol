use std::sync::Arc;

use async_trait::async_trait;
use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::{IndexError, Indexer};
use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Transaction};
use webauthn_rs::prelude::Webauthn;

use crate::chat::ChatBus;
use crate::internal_role::RemoteIndexer;
use crate::nodes::{PeerTable, ShardRegistry};
use crate::presence::PresenceStore;

/// The polymorphic replacement for a bare `PostgresIndexer` on
/// `AppState`, so a Gateway-only deployment (`AVALON_NODE_ROLES` excluding
/// `indexer`) can route every indexer read/write through a remote Indexer
/// role instead of requiring a local one — see `crate::internal_role`'s own
/// module doc comment for the wire protocol this rides on, and
/// `crate::nodes::node_roles`/`indexer_role_is_local` for how a process
/// decides which variant to build.
///
/// Every combined-binary (`AVALON_NODE_ROLES=combined`, the default) call
/// site is unaffected: `Local` behaves byte-for-byte like the bare
/// `PostgresIndexer` this replaced.
#[derive(Clone)]
pub enum IndexerHandle {
    /// A real local `PostgresIndexer` — today's only configuration, and
    /// still every combined-binary deployment's configuration.
    Local(PostgresIndexer),
    /// A `RemoteIndexer` pointed at another process's
    /// `/internal/indexer/*` endpoints — a Gateway-only deployment's
    /// configuration, per `AVALON_INDEXER_REMOTE_URL`.
    Remote(RemoteIndexer),
}

impl IndexerHandle {
    /// The same-transaction, atomic half of an indexer write.
    ///
    /// For [`Self::Local`], this is exactly `PostgresIndexer::apply_in_tx`
    /// as it always was: the app-data write and the projection update
    /// commit or roll back together, inside the caller's own transaction.
    ///
    /// For [`Self::Remote`], this is a deliberate no-op. A `RemoteIndexer`
    /// talks over HTTP — it cannot join a local Postgres transaction, so
    /// true same-transaction atomicity between the app-data write and the
    /// remote projection update is simply not possible for a Gateway-only
    /// deployment. Rather than silently pretending otherwise, this call
    /// becomes the deliberate deferral point: the actual remote apply
    /// happens in [`Self::apply_after_commit`], once the local transaction
    /// this call is nested inside has actually committed.
    pub async fn apply_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
    ) -> Result<(), IndexError> {
        match self {
            IndexerHandle::Local(indexer) => indexer.apply_in_tx(tx, event).await,
            IndexerHandle::Remote(_) => Ok(()),
        }
    }

    /// The eventual-consistency half of an indexer write, called once the
    /// transaction `apply_in_tx` above was nested inside has committed.
    ///
    /// For [`Self::Local`], a no-op — the projection update already
    /// happened, atomically, inside that transaction.
    ///
    /// For [`Self::Remote`], this makes the real HTTP call
    /// (`RemoteIndexer::apply`) that actually applies `event` to the
    /// remote Indexer role's projections. This is the accepted tradeoff of
    /// a Gateway-only deployment: for the short window between the local
    /// transaction committing and this call completing — or for however
    /// long a transient failure here takes to be corrected — the app-data
    /// write is already durable and is the source of truth, but the
    /// remote indexer's projection can lag behind it or, if this call
    /// fails outright, miss it entirely. A failure here is logged loudly
    /// (`tracing::error!`) but deliberately does **not** fail the overall
    /// request: the app-data write already committed, so the request
    /// genuinely succeeded from the caller's point of view. The indexer
    /// can catch up later via `avalon rebuild-index`'s existing
    /// rebuild-from-events guarantee; a background retry/backfill
    /// mechanism for this specific gap is a possible future improvement,
    /// not built here.
    ///
    /// Deliberately infallible (always returns `Ok(())`, even when the
    /// remote call itself failed): every call site uses `?` right after
    /// this, and by design that must never turn a genuinely-committed
    /// app-data write into a failed HTTP response. A caller that needs to
    /// know whether the remote apply itself succeeded (none does today)
    /// should watch this log line rather than this return value.
    pub async fn apply_after_commit(&self, event: &ProtocolEvent) -> Result<(), IndexError> {
        match self {
            IndexerHandle::Local(_) => {}
            IndexerHandle::Remote(remote) => {
                if let Err(err) = remote.apply(event).await {
                    tracing::error!(
                        event_id = %event.id,
                        event_kind = %event.kind,
                        "indexer: remote apply_after_commit failed — app-data write already \
                         committed and remains the source of truth, but the remote indexer's \
                         projection is now behind (will catch up on a future rebuild): {err}"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Lets `IndexerHandle` be used anywhere an `impl Indexer`/`dyn Indexer` is
/// expected (e.g. `crate::internal_role`'s own server-side handlers, which
/// call `state.indexer.apply`/`.rebuild` directly when this node is itself
/// acting as the Indexer role for a remote caller) — delegates to whichever
/// concrete implementation this handle wraps.
#[async_trait]
impl Indexer for IndexerHandle {
    async fn apply(&self, event: &ProtocolEvent) -> Result<(), IndexError> {
        match self {
            IndexerHandle::Local(indexer) => indexer.apply(event).await,
            IndexerHandle::Remote(remote) => remote.apply(event).await,
        }
    }

    async fn rebuild(&self, events: &[ProtocolEvent]) -> Result<(), IndexError> {
        match self {
            IndexerHandle::Local(indexer) => indexer.rebuild(events).await,
            IndexerHandle::Remote(remote) => remote.rebuild(events).await,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub chain: PostgresSettlementProvider,
    /// The query/index layer — `handlers::register_finish`
    /// and `update_profile` (and every other app-data write with a
    /// projection) call `indexer.apply_in_tx` in the same transaction as
    /// the identity/outbox rows, then `indexer.apply_after_commit` right
    /// after that transaction commits. For [`IndexerHandle::Local`] (every
    /// combined-binary deployment) this is exactly the old behavior:
    /// `apply_in_tx` does the real, atomic write and `apply_after_commit`
    /// is a no-op. For [`IndexerHandle::Remote`] (a Gateway-only
    /// deployment) it's the reverse: `apply_in_tx` is a no-op (a
    /// remote indexer can't join this transaction) and
    /// `apply_after_commit` makes the real HTTP call once the app-data
    /// write is durable — an accepted eventual-consistency window, not
    /// full atomicity. See `IndexerHandle`'s own doc comment and
    /// `docs/projects/backend-server/architecture/query-and-indexing.md`.
    pub indexer: IndexerHandle,
    /// Built once at startup from `AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`.
    /// `Webauthn` itself isn't cheap to reconstruct (origin parsing/validation),
    /// so it's shared behind an `Arc` rather than rebuilt per request.
    pub webauthn: Arc<Webauthn>,
    /// In-process, per-node presence state. Never persisted —
    /// see `crate::presence` module docs.
    pub presence: PresenceStore,
    /// Fan-out for guild channel and conversation message push.
    /// Never persisted — Postgres is the source of truth, this only
    /// wakes up an already-subscribed client. See `crate::chat`.
    pub chat: ChatBus,
    /// `AVALON_SETTLEMENT_SUBMIT_KEY` — the bearer credential
    /// `POST /ledger/submit` requires. `None` means the endpoint refuses
    /// every request rather than accepting an unauthenticated one; see
    /// `crate::settlement::submit_ledger_batch`.
    pub settlement_submit_key: Option<String>,
    /// This node's local peer table — in-process,
    /// non-durable, same posture `presence` takes. See `crate::nodes`.
    pub peers: PeerTable,
    /// `AVALON_MANAGED_HOSTING_VERIFY_KEY` — the one hosted
    /// integrator's settlement *public* key, used only to verify a
    /// `POST /ledger/finalize-batch` caller's signature; this node never
    /// holds the corresponding private key. `None` means both
    /// `/ledger/prepare-batch` and `/ledger/finalize-batch` refuse every
    /// request — an operator who never turns on managed hosting gets
    /// endpoints that exist but accept nothing, never ones that are
    /// silently open. **Interim, single-key-per-node**: real
    /// per-shard key resolution (via issuer-key registration) isn't built
    /// yet — see `crate::settlement`'s module doc comment.
    pub managed_hosting_verify_key: Option<ed25519_dalek::VerifyingKey>,
    /// `AVALON_KNOWN_SHARDS`/`AVALON_SHARD_VERIFY_KEYS` —
    /// which shards this node aggregates into a cross-shard root, and how
    /// it verifies each one's STH. `None` (the default) falls back to the
    /// one-shard degenerate case — see `crate::cross_shard`'s own module
    /// doc comment.
    pub known_shards: Option<crate::cross_shard::KnownShardsConfig>,
    /// A handle to the outbox worker's own live
    /// remote-submit failure tracker, for `GET /ledger/remote-submit-status`
    /// (`crate::settlement::remote_submit_status`) to read. `None` when
    /// this node has no `AVALON_SETTLEMENT_REMOTE_URL(S)` configured at
    /// all — it isn't forwarding to any remote authority, so there is
    /// nothing to ever report as failing.
    pub remote_submit_status: Option<crate::outbox::RemoteSubmitStatus>,
    /// `AVALON_OWN_SHARD_ID`, defaulting to `"core"`. Which
    /// shard this node's own `chain`/`ledger_entries` represents, when it
    /// has any local history — see `crate::settlement`'s module doc
    /// comment for why this matters now that a node can hold local
    /// history for one shard while mirroring a *different* one.
    pub own_shard_id: String,
    /// `AVALON_MIRROR_PEERS` parsed into a `shard_id -> url`
    /// map, for `crate::settlement`'s shard-scoped mirror reads. See
    /// `crate::settlement::ShardMirrorSources`'s own doc comment.
    pub shard_mirror_sources: crate::settlement::ShardMirrorSources,
    /// Local guild-channel/conversation subscriber refcounts —
    /// always present (cheap, no config needed) regardless of whether the
    /// DHT itself is enabled, so `crate::chat`'s subscribe handlers have
    /// one code path either way. Only actually reaches the DHT when
    /// `dht_commands` below is `Some`; see `crate::interest`'s own module
    /// doc comment.
    pub interest: crate::interest::InterestRegistry,
    /// `Some` only when `AVALON_DHT_ENABLED` is set — the
    /// handle the relay path (and this node's own interest-refresh
    /// worker) send `crate::dht::DhtCommand`s through. `None` means this
    /// node has no DHT identity at all yet (default-off
    /// posture), so nothing here can look anything up or register
    /// anything either.
    pub dht_commands: Option<crate::dht::DhtCommandSender>,
    /// This node's own `AVALON_NODE_URL`
    /// (`AnnounceConfig::own_base_url`), so `crate::realtime_relay` can
    /// filter its own base URL out of a DHT interest lookup's results —
    /// a node with a local subscriber for the same scope it's relaying
    /// for would otherwise see itself come back from `interest::lookup`
    /// and relay-POST to itself. `None` means this node can't announce
    /// itself anywhere (`AnnounceConfig`'s own degenerate case), so
    /// nothing to filter either.
    pub own_base_url: Option<String>,
    /// This node's own libp2p peer id when the DHT is enabled, used as its
    /// key in overlay routing.
    pub own_libp2p_peer_id: Option<String>,
    /// The optional per-hoster Redis fast-path in front of
    /// interest lookups — `None` unless `AVALON_REDIS_URL` is set (see
    /// `crate::interest::RedisFastPath`'s own module doc comment). Never
    /// required for correctness; the DHT lookup this sits in front of
    /// stays authoritative either way.
    pub interest_redis_fast_path: Option<crate::interest::RedisFastPath>,
    /// Per-verified-principal rate limit, applied once a credential has
    /// been checked. See `crate::principal_limits`.
    pub principal_limiter: crate::principal_limits::PrincipalLimiter,
    /// Shared with `mirror_watcher::run_worker` (when spawned)
    /// so `POST /mirror/notify`'s handler (`crate::mirror_push::notify`)
    /// can wake its poll loop early instead of waiting out the rest of
    /// the current `AVALON_MIRROR_POLL_INTERVAL_SECS` tick. Always
    /// present, same "cheap, no config needed" posture as `interest`
    /// above — a node not running the mirror-watcher at all still has
    /// somewhere harmless for a stray notification to land.
    pub mirror_wake: std::sync::Arc<tokio::sync::Notify>,
    /// Host resource metrics (CPU/memory/disk/process) for
    /// `GET /nodes/status`'s `resources` block — a shared snapshot kept
    /// current by `crate::resources::start_sampler`, always present (no
    /// config needed) and always best-effort. See `crate::resources`'s
    /// own module doc comment.
    pub host_metrics: crate::resources::HostMetricsSampler,
    /// Layer 2: this node's anti-entropy view of every shard it
    /// currently knows exists, gossiped over the same announce mechanism
    /// already runs. Always present (no config needed — the registry
    /// starts empty and grows purely from what peers gossip plus this
    /// node's own authoritative shard, if any). See `crate::nodes::ShardRegistry`'s
    /// own doc comment; consumed by `crate::cross_shard` (aggregation) and
    /// `crate::mirror_watcher` (opt-in auto-mirroring).
    pub shard_registry: ShardRegistry,
    /// This node's bounded, in-memory view of the most
    /// recently gossiped cosigned-head summary per shard/tree_size —
    /// always present (no config needed, same posture `shard_registry`
    /// takes), consumed by `crate::nodes::announce`/`run_worker` (merge)
    /// and `crate::equivocation` (confirmation). See
    /// `crate::nodes::HeadGossipTracker`'s own doc comment for why it's
    /// never folded into `peers`/`shard_registry`.
    pub head_gossip: crate::nodes::HeadGossipTracker,
    /// `AVALON_ADMIN_TOKEN` — the bearer credential
    /// `GET`/`POST /nodes/log-level` require. Deliberately a *separate*
    /// secret from `settlement_submit_key` above — see `crate::admin`'s
    /// own module doc comment for why. `None` means those endpoints refuse
    /// every request, same posture `settlement_submit_key` already
    /// establishes for its own endpoints.
    pub admin_token: Option<String>,
    /// The live handle to this process's own `tracing`
    /// env-filter, built once in `main::init_tracing` — swapping through
    /// this changes what gets logged on the very next call, no restart.
    pub log_reload_handle: crate::admin::LogReloadHandle,
    /// `AVALON_INTERNAL_ROLE_KEY` — the bearer credential
    /// `crate::internal_role`'s operator-internal, node-to-node endpoints
    /// require (e.g. `POST /internal/indexer/apply`). A *third* distinct
    /// shared secret, deliberately never reused from `settlement_submit_key`
    /// or `admin_token` above — same reasoning used to split
    /// `admin_token` from `settlement_submit_key`: each covers a different
    /// trust domain (public mirror-sync write, this-operator's-own-admin,
    /// this-operator's-own-role-to-role RPC), and collapsing any two of
    /// them would let a credential meant for one purpose reach another.
    /// `None` means every request to those endpoints is refused, same
    /// "unset means closed, never silently open" posture the other two
    /// keys already establish.
    pub internal_role_key: Option<String>,
    /// This node's own record of which distinct peers have
    /// confirmed mirroring which shard, kept current by
    /// `crate::replication::run_worker`. Always present (no config
    /// needed, same posture `shard_registry` above already takes) —
    /// starts empty and fills in over the first few poll ticks.
    pub mirror_confirmations: crate::replication::MirrorConfirmationRegistry,
    /// This node's resolved minimum-replication gate
    /// configuration (`AVALON_MIN_MIRROR_CONFIRMATIONS`/
    /// `AVALON_MIRROR_GRACE_PERIOD_HOURS`), read once at startup. See
    /// `crate::replication`'s own module doc comment.
    pub replication_gate: crate::replication::ReplicationGateConfig,
    /// `Some(base_url)` when this process's own `AVALON_NODE_ROLES`
    /// excludes `realtime` — the resolved, already-validated
    /// `AVALON_REALTIME_URL` of the remote Realtime node `presence::presence_ws`/
    /// `chat::chat_ws` proxy every WebSocket connection through instead of
    /// running `handle_presence_socket`/`handle_chat_socket` locally. `None`
    /// (the default, `combined` or any role list that includes `realtime`)
    /// means this process *is* a Realtime role and serves those sockets
    /// itself, exactly as before this issue. See `crate::realtime_proxy`'s
    /// own module doc comment for the connection-topology decision this
    /// implements and why proxy-through-
    /// Gateway was chosen over telling the client to connect to the
    /// Realtime node directly.
    pub realtime_remote_url: Option<String>,
}
