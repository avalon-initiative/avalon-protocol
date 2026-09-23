//! DHT-backed interest registration and lookup — issue #583, part of epic
//! #580 implementing #542's decision. A node with a local subscriber to a
//! guild channel or conversation registers that interest as a
//! [`crate::dht::DhtCommand::PutRecord`] — immediately on first
//! registration (see [`InterestRegistry::register`]'s own doc comment for
//! why waiting on the first [`REFRESH_INTERVAL`] tick isn't good enough),
//! then re-put every `REFRESH_INTERVAL` for as long as at least one local
//! subscriber remains. `crate::realtime_relay::relay_to_peers` (#584) is
//! the one real caller of [`lookup`]/`GetRecord`, looking up who else is
//! interested instead of #539's original "loop over every peer."
//!
//! **Registration has no explicit "deregister"**, by the ticket's own
//! design: a record's TTL ([`RECORD_TTL`], refreshed every
//! [`REFRESH_INTERVAL`]) simply lapses once nothing refreshes it. What
//! *is* tracked locally is a plain refcount per scope
//! ([`InterestRegistry`]) — [`InterestGuard`]'s `Drop` decrements it, so a
//! websocket handler only has to hold one guard per subscription and let
//! normal Rust scoping handle "this connection went away, stop
//! refreshing," the same way #582's peer table handles a departed peer via
//! pruning rather than an explicit "goodbye" message.
//!
//! Interest is tracked at whatever granularity `crate::chat`'s own
//! `ChatUpdate` actually routes on — a guild **channel** id, not a guild
//! id (a guild can have channels with very different subscriber sets), and
//! a conversation id.
//!
//! **Issue #596 reuses this exact mechanism for mirror-sync push
//! addressing.** `InterestScope::Network` (derived deterministically from
//! a `network_id` via [`InterestScope::for_network`]) is what
//! `crate::mirror_watcher` registers once it discovers, via its own
//! verified STH polling, which network(s) it's actually mirroring, and
//! what `crate::mirror_push` looks up from the committing side to resolve
//! which peers to notify. No new registration/lookup path was built —
//! this ticket's whole point is that #583 already solved "peers register
//! interest in a specific thing via the DHT, and the owner looks up who's
//! interested instead of broadcasting" for a different scope kind.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use avalon_protocol::interest_claim::{ClaimedScope, InterestClaim};
use redis::AsyncCommands;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::dht::{DhtCommand, DhtCommandSender};
use crate::state::AppState;

/// Domain-separates this module's DHT keyspace from any other future use
/// of the same swarm (nothing else uses it yet, but #580's epic explicitly
/// anticipates more than just this) — same motivation
/// `avalon_protocol::sth::signing_message`'s own fixed domain tag documents
/// for exactly the same reason: one scheme's key/message shape must never
/// collide with another's.
const KEY_PREFIX: &[u8] = b"avalon-interest-v1:";

/// How often a still-subscribed scope's record is re-put. Half of
/// [`RECORD_TTL`] so at least one refresh always lands before the previous
/// put would otherwise expire, even if a single refresh tick is delayed.
const REFRESH_INTERVAL: Duration = Duration::from_secs(45);

/// How long a single put is valid for without a refresh — three times
/// [`REFRESH_INTERVAL`], same `PRUNE_INTERVAL_MULTIPLE` margin
/// `crate::nodes`'s peer table already uses for exactly the same reason:
/// generous enough that one or two missed refresh ticks (a transient
/// hiccup) never drops a genuinely live subscription out of the DHT.
const RECORD_TTL: Duration = Duration::from_secs(135);

/// Optional per-hoster Redis fast-path in front of the DHT lookup (issue
/// #585, reusing #545's already-decided optional per-hoster Redis —
/// see `crate::redis_limits`'s own module doc comment for that precedent's
/// full "strictly per-hoster, never network-wide" rationale, which applies
/// identically here). [`run_worker`] writes to it on every `PutRecord`
/// (in addition to, never instead of, the real DHT put — Redis is never
/// the only place a registration lives); [`lookup`] checks it first and
/// only falls through to a real DHT `GetRecord` when Redis has no answer,
/// skipping a network hop for the common case where an interested peer is
/// in this operator's own fleet.
///
/// **Never required for correctness.** Unset (the default) or
/// momentarily unreachable both fall straight through to the DHT, exactly
/// #583/#584's original behavior before this ticket — the DHT, not Redis,
/// is the one thing assumed to exist and answer correctly.
///
/// **Known simplification, accepted rather than solved here**, same
/// honesty-note posture `crate::redis_limits`'s own module doc takes for
/// its fixed-window rate limiter: a scope's Redis entry is one `SET` with
/// a single whole-key TTL, not a per-member one, so a fleet member whose
/// own subscriber left keeps appearing in a Redis-fast-path lookup for as
/// long as *any other* fleet member keeps refreshing that same scope.
/// Worst case this produces one extra, harmless relay POST to a node with
/// nobody left to deliver to — never a missed delivery, since the DHT
/// lookup (unaffected by this) remains the source of truth whenever this
/// fast path is unset or empty.
#[derive(Clone)]
pub struct RedisFastPath {
    conn: redis::aio::ConnectionManager,
    /// Prefixed into every key this struct reads or writes — see
    /// [`Self::from_env`]'s own doc comment on why this can't be left
    /// implicit the way [`InterestScope::dht_key`] leaves it.
    network_id: String,
}

impl RedisFastPath {
    /// `None` when `AVALON_REDIS_URL` is unset — every caller here treats
    /// that identically to "Redis didn't have the answer," so nothing
    /// downstream needs its own separate unconfigured-vs-empty branch.
    ///
    /// `network_id` is not optional the way it might look at a glance:
    /// unlike `InterestScope::dht_key`, whose lack of an embedded
    /// `network_id` is safe only because two different networks' libp2p
    /// swarms are already unable to talk to each other at all (a
    /// different `kad` protocol id per network, peers on the wrong one
    /// disconnected on `identify` mismatch — see `crate::dht`), and
    /// unlike Postgres, where each network gets its own separate
    /// database, Redis has no equivalent structural boundary: this
    /// struct's own doc comment already says operators may legitimately
    /// point more than one network's fleet (dev/staging/prod, or
    /// multiple separate deployments) at the *same* Redis instance,
    /// since it's just a cache. Without `network_id` in the key, two
    /// networks sharing one Redis would rely on their scope UUIDs never
    /// coinciding — astronomically unlikely, but a hope, not a
    /// guarantee, and a hard boundary is exactly what "environments must
    /// never cross-contaminate" requires. Every key this struct touches
    /// is therefore namespaced by `network_id` explicitly, so a
    /// misconfigured node pointed at the wrong network's Redis can never
    /// read or write another network's interest data, full stop.
    pub async fn from_env(network_id: &str) -> Option<Self> {
        let url = std::env::var("AVALON_REDIS_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let client =
            redis::Client::open(url).expect("AVALON_REDIS_URL must be a valid redis:// URL");
        let conn = redis::aio::ConnectionManager::new(client).await.expect(
            "failed to connect to AVALON_REDIS_URL — check the Redis instance is reachable",
        );
        Some(Self {
            conn,
            network_id: network_id.to_string(),
        })
    }

    /// Adds `own_base_url` to `scope`'s member set and refreshes the
    /// whole key's TTL — called alongside every real DHT `PutRecord`,
    /// never instead of it (see this struct's own doc comment on why a
    /// Redis-only registration isn't safe to rely on).
    async fn put(&self, scope: InterestScope, own_base_url: &str) {
        let mut conn = self.conn.clone();
        let key = scope.redis_key(&self.network_id);
        let result: redis::RedisResult<()> = async {
            conn.sadd::<_, _, ()>(&key, own_base_url).await?;
            conn.expire::<_, ()>(&key, RECORD_TTL.as_secs() as i64)
                .await?;
            Ok(())
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "avalon-interest: redis fast-path put failed, DHT put still went through");
        }
    }

    /// Every base URL currently in `scope`'s member set, or `None` if
    /// Redis has nothing for it (an empty set and a missing key are the
    /// same "no answer" to a caller, and both are, correctly, not
    /// distinguished from "Redis is momentarily unreachable" either — see
    /// this struct's own doc comment).
    async fn lookup(&self, scope: InterestScope) -> Option<Vec<String>> {
        let mut conn = self.conn.clone();
        match conn
            .smembers::<_, Vec<String>>(scope.redis_key(&self.network_id))
            .await
        {
            Ok(members) if !members.is_empty() => Some(members),
            Ok(_) => None,
            Err(err) => {
                tracing::warn!(error = %err, "avalon-interest: redis fast-path lookup failed, falling through to the DHT");
                None
            }
        }
    }
}

/// What a node can hold local-subscriber interest in — the two shapes
/// `crate::chat::ChatUpdate` actually routes on, plus (issue #596) a
/// mirror's interest in a specific settlement `network_id`'s STH stream.
/// Not `PartialOrd`/`Ord`: nothing here needs to sort scopes, only
/// hash/compare them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterestScope {
    Channel(Uuid),
    Conversation(Uuid),
    /// Issue #596: never constructed directly — see [`Self::for_network`],
    /// which derives this deterministically from a `network_id` string so
    /// `InterestScope` can stay `Copy`/fixed-size like every other variant
    /// rather than growing a `String` payload that would force every
    /// existing call site (registration, lookup, the DHT/Redis worker
    /// loops) off `Copy` for the sake of one variant.
    Network(Uuid),
    /// Epic #623, issue #635: the identity-locator scope — registered by
    /// `crate::identity_locator::run_worker` for every identity this node
    /// durably has (authors or mirrors) `identity_signing_keys` for, so any
    /// node can resolve *every* shard an identity has real history on, not
    /// just wherever `identity.created` happened to land. Same trust model
    /// as `Network` (a bare `own_base_url`, no signed claim): a node
    /// advertising this scope is asserting a fact about its own local data,
    /// not vouching for the identity itself — a consumer still verifies
    /// whatever it actually fetches from a resolved location independently
    /// (issue #636), so a false or stale registration here only ever wastes
    /// a lookup, never a security bypass.
    Identity(Uuid),
}

/// Fixed, arbitrary namespace UUID (issue #596) used only to derive
/// [`InterestScope::Network`] deterministically from a `network_id`
/// string via `Uuid::new_v5` — never persisted or exposed, just a domain
/// separator so two different processes deriving a scope for the same
/// `network_id` (the registering mirror and the looking-up authority)
/// always land on the same value, without needing to store the raw string
/// anywhere.
const NETWORK_SCOPE_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xba, 0x10, 0x0d, 0x59, 0x6e, 0x4c, 0x59, 0x8e, 0x1a, 0x6d, 0x69, 0x72, 0x72, 0x6f, 0x72,
]);

impl InterestScope {
    /// Issue #596: the scope a mirror registers interest in for a given
    /// settlement `network_id` — the same shape a client already registers
    /// interest in a guild channel, extended to mirror-sync push
    /// addressing per that ticket's decided design. Deterministic: calling
    /// this twice with the same `network_id` (whether on the registering
    /// mirror or the looking-up authority) always derives the same
    /// [`InterestScope::Network`].
    pub fn for_network(network_id: &str) -> Self {
        InterestScope::Network(Uuid::new_v5(
            &NETWORK_SCOPE_NAMESPACE,
            network_id.as_bytes(),
        ))
    }

    /// The DHT key this scope's interest is stored under. Kademlia hashes
    /// the key bytes itself for routing (XOR distance over a multihash of
    /// this key, not the raw bytes) — no need to pre-hash here, just make
    /// the input unambiguous, which [`KEY_PREFIX`] plus a tagged variant
    /// byte already does (a `Channel` and a `Conversation` with the same
    /// underlying `Uuid` must never collide). `pub` so
    /// `crates/server/tests/interest_dht.rs` can drive a raw
    /// [`crate::dht::DhtCommand::PutRecord`] under the exact same key
    /// [`run_worker`] would, rather than reimplementing this derivation a
    /// second time and risking the two silently diverging.
    pub fn dht_key(&self) -> Vec<u8> {
        let mut bytes = KEY_PREFIX.to_vec();
        match self {
            InterestScope::Channel(id) => {
                bytes.push(b'c');
                bytes.extend_from_slice(id.as_bytes());
            }
            InterestScope::Conversation(id) => {
                bytes.push(b'd');
                bytes.extend_from_slice(id.as_bytes());
            }
            InterestScope::Network(id) => {
                bytes.push(b'n');
                bytes.extend_from_slice(id.as_bytes());
            }
            InterestScope::Identity(id) => {
                bytes.push(b'i');
                bytes.extend_from_slice(id.as_bytes());
            }
        }
        bytes
    }

    /// This scope's key in the optional Redis fast-path (#585). Unlike
    /// [`dht_key`](Self::dht_key), which can safely omit `network_id`
    /// because the libp2p swarm itself is already network-isolated,
    /// Redis has no such structural boundary — see
    /// [`RedisFastPath::from_env`]'s doc comment — so `network_id` is
    /// embedded in every key here as a hard requirement, not an
    /// afterthought: two networks sharing one Redis instance must never
    /// be able to read or write each other's entries, regardless of how
    /// unlikely a bare-UUID collision would have been.
    fn redis_key(&self, network_id: &str) -> String {
        match self {
            InterestScope::Channel(id) => format!("avalon:interest:{network_id}:channel:{id}"),
            InterestScope::Conversation(id) => {
                format!("avalon:interest:{network_id}:conversation:{id}")
            }
            InterestScope::Network(id) => format!("avalon:interest:{network_id}:network:{id}"),
            InterestScope::Identity(id) => format!("avalon:interest:{network_id}:identity:{id}"),
        }
    }
}

/// In-process refcount per [`InterestScope`] — how `crate::interest::run_worker`
/// knows which scopes to keep refreshing. Cheap to clone (`Arc`-backed),
/// same shape `crate::nodes::PeerTable`/`crate::presence::PresenceStore`
/// already establish for shared in-process state. Never persisted: a
/// restart naturally drops to zero subscribers until clients reconnect and
/// re-subscribe, at which point registration resumes on its own.
#[derive(Clone)]
pub struct InterestRegistry {
    counts: Arc<Mutex<HashMap<InterestScope, usize>>>,
    /// Issue #610: the currently-registered
    /// [`InterestClaim`](avalon_protocol::interest_claim::InterestClaim), wire-
    /// encoded, for each `Channel`/`Conversation` scope that has one —
    /// [`run_worker`] `PutRecord`s these bytes verbatim instead of a bare
    /// `own_base_url` for those two variants (see [`InterestRegistry::dht_value`]).
    /// Never populated for `Network` scope: issue #596's mirror-sync use of
    /// this same mechanism is node-to-node, not identity-scoped, and is
    /// deliberately out of scope for #610 (see `crate::interest_claim`'s
    /// module doc comment). Overwritten, never merged, by each
    /// [`register_with_claim`](Self::register_with_claim) call for the same
    /// scope — any one currently-valid claim is enough to route delivery to
    /// this node, so the most recent registration simply wins.
    claims: Arc<Mutex<HashMap<InterestScope, String>>>,
    /// Fires once a scope goes from zero to one active guard — see
    /// [`register`](Self::register)/[`run_worker`]'s own doc comments for
    /// why a scope can't just wait for the next [`REFRESH_INTERVAL`] tick.
    /// `unbounded`: a `register()` call is synchronous (no `.await` point
    /// to apply backpressure at) and firing rarely enough in practice
    /// (once per newly-active scope, not once per subscriber) that an
    /// unbounded channel's usual downside doesn't apply here.
    newly_active: mpsc::UnboundedSender<InterestScope>,
}

/// Holds one scope's refcount up by one for as long as this guard lives —
/// acquired via [`InterestRegistry::register`], dropped by whatever caller
/// (typically a websocket handler) held it, most often because the
/// connection itself closed. See this module's own doc comment for why
/// there is no corresponding explicit DHT deregister call.
pub struct InterestGuard {
    registry: InterestRegistry,
    scope: InterestScope,
}

impl Drop for InterestGuard {
    fn drop(&mut self) {
        let mut counts = self
            .registry
            .counts
            .lock()
            .expect("interest registry lock poisoned");
        if let Some(count) = counts.get_mut(&self.scope) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.scope);
                // Only clear a stored claim once the scope has no active
                // guard left at all — a different still-live guard for the
                // same scope (e.g. a second connection subscribed to the
                // same channel) may have registered its own claim, and this
                // guard dropping must never blow that one away.
                self.registry
                    .claims
                    .lock()
                    .expect("interest registry lock poisoned")
                    .remove(&self.scope);
            }
        }
    }
}

impl InterestRegistry {
    /// Returns the registry plus the receiving half of its "newly active"
    /// signal — [`run_worker`] is the one intended consumer, handed this
    /// receiver once at startup (see `main.rs`'s wiring), the same
    /// "constructor hands back the one channel half its caller needs"
    /// shape `crate::dht::start`/`DhtHandle` already establish. Dropping
    /// the receiver without ever running `run_worker` (e.g.
    /// `AVALON_DHT_ENABLED` unset) is fine — every future `register()`
    /// still tracks refcounts correctly, its `send` just has nowhere to
    /// go and is ignored.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<InterestScope>) {
        let (newly_active, receiver) = mpsc::unbounded_channel();
        (
            Self {
                counts: Arc::new(Mutex::new(HashMap::new())),
                claims: Arc::new(Mutex::new(HashMap::new())),
                newly_active,
            },
            receiver,
        )
    }

    /// Registers one local subscriber's interest in `scope`, returning a
    /// guard that un-registers it on drop. Multiple guards for the same
    /// scope (e.g. two different local connections both watching the same
    /// channel) are independent — the scope stays registered until every
    /// guard for it has dropped. The scope's first registration (0 -> 1)
    /// signals [`run_worker`] to `PutRecord` immediately rather than
    /// waiting out a full [`REFRESH_INTERVAL`] — live-testing this against
    /// a real relay (`crates/server/tests/realtime_relay.rs`) found a
    /// fresh subscription otherwise invisible to a lookup for up to 45s,
    /// far too slow to be useful.
    ///
    /// Only ever used for [`InterestScope::Network`] (issue #596's mirror
    /// registration — never identity-scoped, see this struct's own
    /// `claims` field doc comment). `Channel`/`Conversation` registration
    /// goes through [`register_with_claim`](Self::register_with_claim)
    /// instead as of issue #610.
    pub fn register(&self, scope: InterestScope) -> InterestGuard {
        self.bump(scope)
    }

    /// Registers one local subscriber's interest in a `Channel`/
    /// `Conversation` `scope` (issue #610), storing `claim` (the wire-
    /// encoded, already-verified [`InterestClaim`] — see
    /// `crate::interest::verify_claim`, which every caller of this must run
    /// first) as exactly what [`run_worker`] will `PutRecord` for as long as
    /// this guard, or another guard for the same scope, stays alive. Stored
    /// before the refcount bump below so a concurrent `run_worker` reacting
    /// to the immediate `newly_active` signal a fresh 0 -> 1 registration
    /// sends never finds `scope` active with no claim to put yet.
    pub fn register_with_claim(&self, scope: InterestScope, claim: String) -> InterestGuard {
        self.claims
            .lock()
            .expect("interest registry lock poisoned")
            .insert(scope, claim);
        self.bump(scope)
    }

    fn bump(&self, scope: InterestScope) -> InterestGuard {
        let mut counts = self.counts.lock().expect("interest registry lock poisoned");
        let count = counts.entry(scope).or_insert(0);
        *count += 1;
        if *count == 1 {
            let _ = self.newly_active.send(scope);
        }
        InterestGuard {
            registry: self.clone(),
            scope,
        }
    }

    /// The bytes [`run_worker`] should `PutRecord` for `scope` right now, or
    /// `None` when there's nothing valid to advertise yet — a `Channel`/
    /// `Conversation` scope between [`bump`]'s refcount increment and
    /// [`register_with_claim`]'s claim being stored (shouldn't be
    /// observable given the storage-before-bump order above, but `run_worker`
    /// treats it as "skip this tick, try again next refresh" rather than
    /// panicking either way), or a `Network` scope, which was never
    /// claim-based to begin with (issue #596, unaffected by #610 — see
    /// `claims`' own doc comment) and just advertises `own_base_url`
    /// directly, exactly as before this ticket.
    fn dht_value(&self, scope: InterestScope, own_base_url: &str) -> Option<Vec<u8>> {
        match scope {
            InterestScope::Channel(_) | InterestScope::Conversation(_) => self
                .claims
                .lock()
                .expect("interest registry lock poisoned")
                .get(&scope)
                .map(|claim| claim.as_bytes().to_vec()),
            InterestScope::Network(_) | InterestScope::Identity(_) => {
                Some(own_base_url.as_bytes().to_vec())
            }
        }
    }

    /// Every scope with at least one live guard right now — what
    /// [`run_worker`] refreshes on each tick.
    fn active_scopes(&self) -> Vec<InterestScope> {
        self.counts
            .lock()
            .expect("interest registry lock poisoned")
            .keys()
            .copied()
            .collect()
    }

    #[cfg(test)]
    fn count(&self, scope: InterestScope) -> usize {
        self.counts
            .lock()
            .expect("interest registry lock poisoned")
            .get(&scope)
            .copied()
            .unwrap_or(0)
    }
}

/// Looks up who currently has a local subscriber for `scope`, decoding
/// each stored value as a UTF-8 base URL (silently dropping anything that
/// doesn't decode — never expected from this module's own
/// [`run_worker`], but a malformed or malicious record from elsewhere in
/// the DHT shouldn't be fatal to the caller, same posture
/// `crate::dht::new_dht_peer` already takes for a bad peer-table entry).
/// Deduplicated: live-testing this against two real nodes
/// (`crates/server/tests/interest_dht.rs`) found `get_record` reporting
/// the same value more than once (once per peer that happened to answer
/// with a copy) — #584's relay path needs a plain "who to notify" list,
/// not one entry per DHT replica that happened to respond.
/// `dht_commands` being unavailable to the caller (e.g. `AVALON_DHT_ENABLED`
/// unset) is the caller's own concern — this function assumes a live
/// channel.
///
/// #584 is the ticket that actually calls this from the relay path; #583
/// only has to prove it works. `redis_fast_path`, when `Some` (#585),
/// is checked first — a non-empty answer from it skips the DHT `GetRecord`
/// entirely; `None`/empty falls straight through to the DHT exactly as if
/// no fast path were configured at all.
pub async fn lookup(
    dht_commands: &DhtCommandSender,
    scope: InterestScope,
    redis_fast_path: Option<&RedisFastPath>,
) -> Vec<String> {
    if let Some(redis_fast_path) = redis_fast_path {
        if let Some(mut members) = redis_fast_path.lookup(scope).await {
            members.sort_unstable();
            members.dedup();
            return members;
        }
    }

    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    if dht_commands
        .send(DhtCommand::GetRecord {
            key: scope.dht_key(),
            respond_to,
        })
        .await
        .is_err()
    {
        // The DHT worker task is gone — only happens during process
        // shutdown (see `dht::run_worker`'s own doc comment on the
        // matching case for `PutRecord`/`GetRecord` senders).
        return Vec::new();
    }
    let values = receiver.await.unwrap_or_default();
    let mut base_urls: Vec<String> = values
        .into_iter()
        .filter_map(|v| String::from_utf8(v).ok())
        .collect();
    base_urls.sort_unstable();
    base_urls.dedup();
    base_urls
}

/// Issue #610's replacement for [`lookup`] on `Channel`/`Conversation`
/// scopes — `crate::realtime_relay`'s only caller for those two variants
/// now, `lookup` itself remaining exactly as it was for `crate::mirror_push`'s
/// `Network`-scope use (see `crate::interest_claim`'s module doc comment on
/// why that path is out of scope here). Decodes each raw DHT value as a
/// wire-encoded [`InterestClaim`] rather than a bare base URL, and only
/// returns a claim's `base_url` once this node has independently verified
/// both its signature ([`verify_claim_signature`]) *and*, separately, that
/// the claim's own `identity_id` is still a real member of `scope` right
/// now — checked fresh against this node's own local, ledger-derived
/// membership tables (`crate::channels::is_member_of_channel` /
/// `crate::conversations::require_unblocked_participant`), never trusted
/// from the DHT record itself. A claim that fails either check is silently
/// dropped, exactly like a malformed record already was before this ticket
/// — a forged/stale registration and "nobody's actually interested" must
/// look identical to this relay path.
///
/// **No Redis fast-path** (#585): that cache only ever stored a bare
/// `own_base_url` string (see `RedisFastPath::put`'s call sites in
/// [`run_worker`], unchanged by this ticket) with no claim/signature
/// attached to verify — trusting it here would silently reopen exactly the
/// hole this function exists to close. Every `Channel`/`Conversation`
/// lookup now always takes the real DHT `GetRecord` round trip.
pub async fn lookup_claimed(
    state: &AppState,
    dht_commands: &DhtCommandSender,
    scope: InterestScope,
) -> Vec<String> {
    let (respond_to, receiver) = tokio::sync::oneshot::channel();
    if dht_commands
        .send(DhtCommand::GetRecord {
            key: scope.dht_key(),
            respond_to,
        })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let values = receiver.await.unwrap_or_default();

    let mut base_urls = Vec::new();
    for value in values {
        let Ok(wire) = String::from_utf8(value) else {
            continue;
        };
        let Some(claim) = verify_claim_signature(state, &wire, scope).await else {
            continue;
        };
        let still_a_member = match claim.scope {
            ClaimedScope::Channel { channel_id } => {
                crate::channels::is_member_of_channel(state, channel_id, claim.identity_id)
                    .await
                    .unwrap_or(false)
            }
            ClaimedScope::Conversation { conversation_id } => {
                crate::conversations::require_unblocked_participant(
                    state,
                    conversation_id,
                    claim.identity_id,
                )
                .await
                .is_ok()
            }
        };
        if still_a_member {
            base_urls.push(claim.base_url);
        }
    }
    base_urls.sort_unstable();
    base_urls.dedup();
    base_urls
}

/// Small allowance for clock drift between the minting client and this
/// node, same rationale (and value) as
/// `crate::continuation::CLOCK_SKEW_ALLOWANCE`.
const CLOCK_SKEW_ALLOWANCE: time::Duration = time::Duration::seconds(5);

/// Core verification shared by both call sites below: well-formed JSON,
/// names `expected_scope`, a validity window that hasn't expired, isn't
/// claiming to have been issued in the future beyond clock-skew allowance,
/// isn't longer than the protocol's own documented cap, and carries a
/// signature that actually verifies against `signing_key_id`'s current
/// (non-revoked) public key. Does **not** check `identity_id` against any
/// expected caller, and does **not** check `base_url` against anything —
/// callers that need either do so themselves, since the two real callers
/// need opposite things here (see [`verify_claim`] and
/// [`lookup_claimed`]'s own doc comments).
async fn verify_claim_signature(
    state: &AppState,
    wire: &str,
    expected_scope: InterestScope,
) -> Option<InterestClaim> {
    let claim: InterestClaim = serde_json::from_str(wire).ok()?;

    let scope_matches = match (claim.scope, expected_scope) {
        (ClaimedScope::Channel { channel_id }, InterestScope::Channel(expected)) => {
            channel_id == expected
        }
        (ClaimedScope::Conversation { conversation_id }, InterestScope::Conversation(expected)) => {
            conversation_id == expected
        }
        _ => false,
    };
    if !scope_matches {
        return None;
    }

    let now = OffsetDateTime::now_utc();
    if claim.expires_at < now || claim.issued_at > now + CLOCK_SKEW_ALLOWANCE {
        return None;
    }
    if claim.expires_at - claim.issued_at
        > time::Duration::seconds(avalon_protocol::interest_claim::DEFAULT_TTL_SECONDS)
    {
        return None;
    }

    let key = avalon_indexer::projections::identity_signing_keys::find_active_by_id(
        &state.pool,
        claim.signing_key_id,
    )
    .await
    .ok()??;
    // The verified key's own `identity_id`, never the claim's claimed one —
    // same posture `crate::continuation::verify` already takes for exactly
    // the same reason.
    if key.identity_id != claim.identity_id {
        return None;
    }
    let signature_bytes = hex::decode(&claim.signature).ok()?;
    if !verify_event_signature(&key.public_key, &claim.signing_bytes(), &signature_bytes) {
        return None;
    }

    Some(claim)
}

/// Registration-side verification (issue #610) — run by `crate::chat`
/// before ever calling [`InterestRegistry::register_with_claim`]. Beyond
/// [`verify_claim_signature`]'s checks, requires `claim.identity_id` to
/// equal `expected_identity` (the already-authenticated websocket caller —
/// a connection only ever registers a claim for the identity that
/// authenticated it, never on behalf of some other identity whose claim it
/// happened to be handed) and `claim.base_url` to equal this node's own
/// `own_base_url` exactly. That second check is the one that actually
/// closes #610's redirection risk: `base_url` lives *inside* the signed
/// bytes precisely so a client can't have it silently rewritten later, but
/// nothing stops a malicious or compromised client from self-signing a
/// claim naming some *other* node's `base_url` in the first place — this
/// node must refuse to register a claim that isn't actually naming itself,
/// or it would just be relaying a genuine member's forged registration
/// toward an attacker-controlled destination instead of that member's own
/// node.
pub async fn verify_claim(
    state: &AppState,
    wire: &str,
    expected_scope: InterestScope,
    expected_identity: Uuid,
) -> Option<InterestClaim> {
    let claim = verify_claim_signature(state, wire, expected_scope).await?;
    if claim.identity_id != expected_identity {
        return None;
    }
    if state.own_base_url.as_deref() != Some(claim.base_url.as_str()) {
        return None;
    }
    Some(claim)
}

fn put_command(scope: InterestScope, value: Vec<u8>, ttl: Duration) -> DhtCommand {
    DhtCommand::PutRecord {
        key: scope.dht_key(),
        value,
        ttl,
    }
}

/// Never returns. `PutRecord`s `own_base_url` under a scope immediately
/// when [`InterestRegistry::register`] signals it just went active (via
/// `newly_active`, the receiver [`InterestRegistry::new`] hands back), and
/// again every [`REFRESH_INTERVAL`] for every still-active scope — the one
/// thing this node ever advertises into the DHT for interest routing,
/// reusing the exact same `base_url` identity #362's HTTP peer table
/// already announces rather than inventing a second way to name this node.
/// `None` (matching `AnnounceConfig::own_base_url`'s own "can be announced
/// TO but can't announce" degenerate case) means this node has nothing
/// useful to put — the worker still runs so it starts advertising
/// correctly the moment `AVALON_NODE_URL` is set and the process restarts,
/// but does nothing, on a tick or otherwise, until then.
pub async fn run_worker(
    registry: InterestRegistry,
    mut newly_active: mpsc::UnboundedReceiver<InterestScope>,
    dht_commands: DhtCommandSender,
    own_base_url: Option<String>,
    redis_fast_path: Option<RedisFastPath>,
) {
    let Some(own_base_url) = own_base_url else {
        tracing::warn!(
            "avalon-interest: AVALON_NODE_URL unset — this node can register local subscribers' \
             interest for others to find, but has no reachable base URL to advertise, so it \
             never will"
        );
        return;
    };

    let mut refresh_interval = tokio::time::interval(REFRESH_INTERVAL);
    loop {
        tokio::select! {
            _ = refresh_interval.tick() => {
                for scope in registry.active_scopes() {
                    let Some(value) = registry.dht_value(scope, &own_base_url) else {
                        // A `Channel`/`Conversation` scope with no claim
                        // stored yet — see `InterestRegistry::dht_value`'s
                        // own doc comment on when this can happen. Skip
                        // this tick, try again next refresh.
                        continue;
                    };
                    // Issue #610: the Redis fast-path (#585) only ever
                    // cached a bare `own_base_url`, never a verifiable
                    // claim — `interest::lookup_claimed` no longer
                    // consults it at all for `Channel`/`Conversation`
                    // scopes (see that function's own doc comment), so
                    // populating it for those scopes now would just be
                    // dead writes.
                    if matches!(scope, InterestScope::Network(_) | InterestScope::Identity(_)) {
                        if let Some(redis_fast_path) = &redis_fast_path {
                            redis_fast_path.put(scope, &own_base_url).await;
                        }
                    }
                    if dht_commands.send(put_command(scope, value, RECORD_TTL)).await.is_err() {
                        // The DHT worker is gone (process shutting down) —
                        // no point looping further, every future send
                        // would hit the same closed channel.
                        return;
                    }
                }
            }
            scope = newly_active.recv() => {
                let Some(scope) = scope else {
                    // Every `InterestRegistry` clone (and so every sender
                    // half) is gone — only happens alongside the whole
                    // process tearing down, same as the channel-closed
                    // case above.
                    return;
                };
                let Some(value) = registry.dht_value(scope, &own_base_url) else {
                    continue;
                };
                if matches!(scope, InterestScope::Network(_) | InterestScope::Identity(_)) {
                    if let Some(redis_fast_path) = &redis_fast_path {
                        redis_fast_path.put(scope, &own_base_url).await;
                    }
                }
                if dht_commands.send(put_command(scope, value, RECORD_TTL)).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_and_conversation_keys_never_collide_for_the_same_uuid() {
        let id = Uuid::new_v4();
        assert_ne!(
            InterestScope::Channel(id).dht_key(),
            InterestScope::Conversation(id).dht_key()
        );
    }

    #[test]
    fn identity_scope_keys_never_collide_with_another_variant_of_the_same_uuid() {
        let id = Uuid::new_v4();
        let identity_key = InterestScope::Identity(id).dht_key();
        assert_ne!(identity_key, InterestScope::Channel(id).dht_key());
        assert_ne!(identity_key, InterestScope::Conversation(id).dht_key());
        assert_ne!(identity_key, InterestScope::Network(id).dht_key());
    }

    #[test]
    fn different_ids_of_the_same_variant_never_collide() {
        let a = InterestScope::Channel(Uuid::new_v4());
        let b = InterestScope::Channel(Uuid::new_v4());
        assert_ne!(a.dht_key(), b.dht_key());
    }

    #[test]
    fn the_same_scope_always_derives_the_same_key() {
        let id = Uuid::new_v4();
        assert_eq!(
            InterestScope::Channel(id).dht_key(),
            InterestScope::Channel(id).dht_key()
        );
    }

    // Issue #596: the whole point of `for_network` deriving a `Uuid`
    // rather than accepting a random one is that two independent callers
    // (the registering mirror, the looking-up authority) computing this
    // scope for the same `network_id` string must always land on the same
    // DHT key.
    #[test]
    fn for_network_is_deterministic_for_the_same_network_id() {
        assert_eq!(
            InterestScope::for_network("avalon-dev-local").dht_key(),
            InterestScope::for_network("avalon-dev-local").dht_key()
        );
    }

    #[test]
    fn for_network_differs_across_network_ids() {
        assert_ne!(
            InterestScope::for_network("avalon-dev-local").dht_key(),
            InterestScope::for_network("avalon-mainnet-1").dht_key()
        );
    }

    #[test]
    fn network_scope_never_collides_with_channel_or_conversation() {
        let id = Uuid::new_v4();
        let network_scope = InterestScope::for_network(&id.to_string());
        assert_ne!(
            network_scope.dht_key(),
            InterestScope::Channel(id).dht_key()
        );
        assert_ne!(
            network_scope.dht_key(),
            InterestScope::Conversation(id).dht_key()
        );
    }

    #[test]
    fn redis_key_never_collides_between_channel_and_conversation() {
        let id = Uuid::new_v4();
        assert_ne!(
            InterestScope::Channel(id).redis_key("avalon-dev-local"),
            InterestScope::Conversation(id).redis_key("avalon-dev-local")
        );
    }

    #[test]
    fn redis_key_is_deterministic_for_the_same_scope() {
        let id = Uuid::new_v4();
        assert_eq!(
            InterestScope::Channel(id).redis_key("avalon-dev-local"),
            InterestScope::Channel(id).redis_key("avalon-dev-local")
        );
    }

    #[test]
    fn redis_key_never_collides_across_network_ids() {
        let id = Uuid::new_v4();
        assert_ne!(
            InterestScope::Channel(id).redis_key("avalon-dev-local"),
            InterestScope::Channel(id).redis_key("avalon-mainnet-1"),
            "the same scope UUID in two different networks must never share a Redis key"
        );
    }

    #[test]
    fn registering_twice_for_the_same_scope_keeps_it_active_until_both_drop() {
        let (registry, _newly_active) = InterestRegistry::new();
        let scope = InterestScope::Channel(Uuid::new_v4());

        let first = registry.register(scope);
        let second = registry.register(scope);
        assert_eq!(registry.count(scope), 2);

        drop(first);
        assert_eq!(registry.count(scope), 1);
        assert!(registry.active_scopes().contains(&scope));

        drop(second);
        assert_eq!(registry.count(scope), 0);
        assert!(!registry.active_scopes().contains(&scope));
    }

    #[test]
    fn unrelated_scopes_do_not_affect_each_others_count() {
        let (registry, _newly_active) = InterestRegistry::new();
        let a = InterestScope::Channel(Uuid::new_v4());
        let b = InterestScope::Conversation(Uuid::new_v4());

        let _guard_a = registry.register(a);
        assert_eq!(registry.count(a), 1);
        assert_eq!(registry.count(b), 0);
    }

    #[test]
    fn the_first_registration_of_a_scope_signals_newly_active() {
        let (registry, mut newly_active) = InterestRegistry::new();
        let scope = InterestScope::Channel(Uuid::new_v4());

        let _guard = registry.register(scope);
        assert_eq!(newly_active.try_recv(), Ok(scope));
    }

    #[test]
    fn a_second_registration_of_an_already_active_scope_does_not_signal_again() {
        let (registry, mut newly_active) = InterestRegistry::new();
        let scope = InterestScope::Channel(Uuid::new_v4());

        let _first = registry.register(scope);
        newly_active.try_recv().expect("first registration signals");

        let _second = registry.register(scope);
        assert!(newly_active.try_recv().is_err());
    }

    // Issue #610's own live acceptance test: a claim with a perfectly valid
    // signature must still be rejected by `lookup_claimed` unless its own
    // `identity_id` is a *current* member of the channel it names — the
    // actual leak #608 left open (an already-admitted node registering
    // interest in a channel/conversation it has no real member in). Needs a
    // real Postgres (`indexer_identity_signing_keys`/`indexer_guild_members`)
    // but no real libp2p swarm — a tiny in-process fake stands in for
    // `crate::dht::run_worker` on the "GetRecord returns whatever was last
    // PutRecord'd under this key" level `lookup_claimed` actually depends
    // on, exactly like `crates/server/tests/interest_dht.rs` proves that
    // primitive itself against two real swarms.
    mod claim_verification_live {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use avalon_protocol::interest_claim::{signing_bytes, ClaimedScope, InterestClaim};
        use ed25519_dalek::{Signer, SigningKey};
        use sqlx::postgres::PgPoolOptions;
        use sqlx::PgPool;

        use super::*;
        use crate::state::AppState;

        async fn test_pool() -> PgPool {
            let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
            PgPoolOptions::new()
                .connect(&database_url)
                .await
                .expect("failed to connect to Postgres — is it reachable?")
        }

        /// Same throwaway-`AppState` construction `crate::authz`'s own test
        /// module establishes, minus the outbox worker and real WebAuthn
        /// env vars — nothing here performs a ceremony.
        async fn test_state(pool: PgPool, own_base_url: Option<String>) -> AppState {
            let webauthn = Arc::new(
                crate::auth::build_webauthn("localhost", "http://localhost:8080")
                    .expect("failed to build a throwaway Webauthn instance for this test"),
            );
            let chain = avalon_chain::PostgresSettlementProvider::new(pool.clone(), "avalon-test");
            let indexer = avalon_indexer::postgres::PostgresIndexer::new(pool.clone());
            AppState {
                pool,
                chain,
                indexer: crate::state::IndexerHandle::Local(indexer),
                webauthn,
                presence: crate::presence::PresenceStore::from_env(),
                chat: crate::chat::ChatBus::new(),
                settlement_submit_key: None,
                peers: crate::nodes::PeerTable::new(),
                managed_hosting_verify_key: None,
                known_shards: None,
                remote_submit_status: None,
                own_shard_id: "core".to_string(),
                shard_mirror_sources: crate::settlement::ShardMirrorSources::default(),
                interest: InterestRegistry::new().0,
                dht_commands: None,
                own_base_url,
                interest_redis_fast_path: None,
                mirror_wake: Arc::new(tokio::sync::Notify::new()),
                host_metrics: crate::resources::HostMetricsSampler::new(Vec::new()),
                shard_registry: crate::nodes::ShardRegistry::new(),
                admin_token: None,
                log_reload_handle: tracing_subscriber::reload::Layer::new(
                    tracing_subscriber::EnvFilter::new("info"),
                )
                .1,
                internal_role_key: None,
                mirror_confirmations: crate::replication::MirrorConfirmationRegistry::new(),
                replication_gate: crate::replication::ReplicationGateConfig::from_env(),
                realtime_remote_url: None,
            }
        }

        /// A minimal fake standing in for a real DHT swarm's `PutRecord`/
        /// `GetRecord` handling (`crate::dht::run_worker`'s own real
        /// version) — this test isn't exercising the network primitive
        /// itself (that's `interest_dht.rs`'s job), only what
        /// `lookup_claimed` does with whatever a `GetRecord` happens to
        /// return.
        fn fake_dht() -> DhtCommandSender {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<DhtCommand>(8);
            let store: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
            tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    match command {
                        DhtCommand::PutRecord { key, value, .. } => {
                            store.lock().unwrap().insert(key, value);
                        }
                        DhtCommand::GetRecord { key, respond_to } => {
                            let values = store
                                .lock()
                                .unwrap()
                                .get(&key)
                                .cloned()
                                .into_iter()
                                .collect();
                            let _ = respond_to.send(values);
                        }
                    }
                }
            });
            tx
        }

        async fn seed_identity_with_signing_key(pool: &PgPool) -> (Uuid, Uuid, SigningKey) {
            let identity_id = Uuid::new_v4();
            sqlx::query("INSERT INTO identities (id) VALUES ($1)")
                .bind(identity_id)
                .execute(pool)
                .await
                .expect("failed to seed identity");

            let signing_key = SigningKey::generate(&mut rand::rng());
            let signing_key_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO indexer_identity_signing_keys \
                 (signing_key_id, identity_id, public_key, added_at) VALUES ($1, $2, $3, now())",
            )
            .bind(signing_key_id)
            .bind(identity_id)
            .bind(signing_key.verifying_key().to_bytes().to_vec())
            .execute(pool)
            .await
            .expect("failed to seed signing key");

            (identity_id, signing_key_id, signing_key)
        }

        async fn seed_guild_channel(pool: &PgPool) -> (Uuid, Uuid) {
            let owner_id = Uuid::new_v4();
            sqlx::query("INSERT INTO identities (id) VALUES ($1)")
                .bind(owner_id)
                .execute(pool)
                .await
                .expect("failed to seed guild owner identity");

            let guild_id = Uuid::new_v4();
            // `tag` is unique (case-insensitively) and capped at 5 chars —
            // a fixed literal collides across this file's two tests
            // (and any repeat run), so it's derived from `guild_id` instead.
            let tag = guild_id.simple().to_string()[..5].to_uppercase();
            sqlx::query("INSERT INTO guilds (id, name, tag, owner) VALUES ($1, $2, $3, $4)")
                .bind(guild_id)
                .bind(format!("interest-claim-test-{guild_id}"))
                .bind(tag)
                .bind(owner_id)
                .execute(pool)
                .await
                .expect("failed to seed guild");

            let channel_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO guild_channels (id, guild_id, name) VALUES ($1, $2, 'general')",
            )
            .bind(channel_id)
            .bind(guild_id)
            .execute(pool)
            .await
            .expect("failed to seed channel");

            (guild_id, channel_id)
        }

        async fn add_member(pool: &PgPool, guild_id: Uuid, identity_id: Uuid) {
            sqlx::query(
                "INSERT INTO indexer_guild_members (guild_id, identity_id, role_index, joined_at) \
                 VALUES ($1, $2, 0, now())",
            )
            .bind(guild_id)
            .bind(identity_id)
            .execute(pool)
            .await
            .expect("failed to seed membership projection row");
        }

        fn mint_claim(
            identity_id: Uuid,
            signing_key_id: Uuid,
            signing_key: &SigningKey,
            channel_id: Uuid,
            base_url: &str,
        ) -> String {
            let scope = ClaimedScope::Channel { channel_id };
            let nonce = Uuid::new_v4();
            let issued_at = OffsetDateTime::now_utc();
            let expires_at = issued_at + time::Duration::hours(1);
            let bytes = signing_bytes(
                identity_id,
                signing_key_id,
                scope,
                base_url,
                nonce,
                issued_at,
                expires_at,
            );
            let signature = signing_key.sign(&bytes);
            let claim = InterestClaim {
                identity_id,
                signing_key_id,
                scope,
                base_url: base_url.to_string(),
                nonce,
                issued_at,
                expires_at,
                signature: hex::encode(signature.to_bytes()),
            };
            serde_json::to_string(&claim).expect("InterestClaim always serializes")
        }

        #[tokio::test]
        #[ignore]
        async fn a_claim_naming_a_channel_the_identity_never_joined_is_rejected() {
            let pool = test_pool().await;
            let dht_commands = fake_dht();
            let (identity_id, signing_key_id, signing_key) =
                seed_identity_with_signing_key(&pool).await;
            let (_guild_id, channel_id) = seed_guild_channel(&pool).await;
            // Deliberately never added as a member of the guild that owns
            // `channel_id`.

            let base_url = "http://attacker-controlled.example";
            let claim = mint_claim(
                identity_id,
                signing_key_id,
                &signing_key,
                channel_id,
                base_url,
            );
            let scope = InterestScope::Channel(channel_id);
            dht_commands
                .send(put_command(
                    scope,
                    claim.into_bytes(),
                    Duration::from_secs(60),
                ))
                .await
                .expect("fake dht channel should still be open");

            let state = test_state(pool, Some(base_url.to_string())).await;
            let found = lookup_claimed(&state, &dht_commands, scope).await;
            assert!(
                found.is_empty(),
                "a claim with a perfectly valid signature must still be rejected once its \
                 identity is not actually a member of the channel it names"
            );
        }

        #[tokio::test]
        #[ignore]
        async fn a_claim_from_a_real_member_is_accepted() {
            let pool = test_pool().await;
            let dht_commands = fake_dht();
            let (identity_id, signing_key_id, signing_key) =
                seed_identity_with_signing_key(&pool).await;
            let (guild_id, channel_id) = seed_guild_channel(&pool).await;
            add_member(&pool, guild_id, identity_id).await;

            let base_url = "http://legitimate-subscriber-node.example";
            let claim = mint_claim(
                identity_id,
                signing_key_id,
                &signing_key,
                channel_id,
                base_url,
            );
            let scope = InterestScope::Channel(channel_id);
            dht_commands
                .send(put_command(
                    scope,
                    claim.into_bytes(),
                    Duration::from_secs(60),
                ))
                .await
                .expect("fake dht channel should still be open");

            let state = test_state(pool, Some(base_url.to_string())).await;
            let found = lookup_claimed(&state, &dht_commands, scope).await;
            assert_eq!(
                found,
                vec![base_url.to_string()],
                "a claim signed by a real, current member should be trusted"
            );
        }
    }
}
