use std::sync::Arc;

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PostgresIndexer;
use sqlx::PgPool;
use webauthn_rs::prelude::Webauthn;

use crate::chat::ChatBus;
use crate::nodes::PeerTable;
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
}
