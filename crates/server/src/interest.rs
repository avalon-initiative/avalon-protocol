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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use redis::AsyncCommands;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::dht::{DhtCommand, DhtCommandSender};

/// Domain-separates this module's DHT keyspace from any other future use
/// of the same swarm (nothing else uses it yet, but #580's epic explicitly
/// anticipates more than just this) — same motivation
/// `avalon_chain::sth::signing_message`'s own fixed domain tag documents
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
}

impl RedisFastPath {
    /// `None` when `AVALON_REDIS_URL` is unset — every caller here treats
    /// that identically to "Redis didn't have the answer," so nothing
    /// downstream needs its own separate unconfigured-vs-empty branch.
    pub async fn from_env() -> Option<Self> {
        let url = std::env::var("AVALON_REDIS_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let client =
            redis::Client::open(url).expect("AVALON_REDIS_URL must be a valid redis:// URL");
        let conn = redis::aio::ConnectionManager::new(client).await.expect(
            "failed to connect to AVALON_REDIS_URL — check the Redis instance is reachable",
        );
        Some(Self { conn })
    }

    /// Adds `own_base_url` to `scope`'s member set and refreshes the
    /// whole key's TTL — called alongside every real DHT `PutRecord`,
    /// never instead of it (see this struct's own doc comment on why a
    /// Redis-only registration isn't safe to rely on).
    async fn put(&self, scope: InterestScope, own_base_url: &str) {
        let mut conn = self.conn.clone();
        let key = scope.redis_key();
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
        match conn.smembers::<_, Vec<String>>(scope.redis_key()).await {
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
/// `crate::chat::ChatUpdate` actually routes on. Not `PartialOrd`/`Ord`:
/// nothing here needs to sort scopes, only hash/compare them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterestScope {
    Channel(Uuid),
    Conversation(Uuid),
}

impl InterestScope {
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
        }
        bytes
    }

    /// This scope's key in the optional Redis fast-path (#585) — same
    /// namespacing intent as [`dht_key`](Self::dht_key), just a plain
    /// string since Redis keys are conventionally that, not raw bytes.
    fn redis_key(&self) -> String {
        match self {
            InterestScope::Channel(id) => format!("avalon:interest:channel:{id}"),
            InterestScope::Conversation(id) => format!("avalon:interest:conversation:{id}"),
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
    pub fn register(&self, scope: InterestScope) -> InterestGuard {
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

fn put_command(scope: InterestScope, own_base_url: &str, ttl: Duration) -> DhtCommand {
    DhtCommand::PutRecord {
        key: scope.dht_key(),
        value: own_base_url.as_bytes().to_vec(),
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
                    if let Some(redis_fast_path) = &redis_fast_path {
                        redis_fast_path.put(scope, &own_base_url).await;
                    }
                    if dht_commands.send(put_command(scope, &own_base_url, RECORD_TTL)).await.is_err() {
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
                if let Some(redis_fast_path) = &redis_fast_path {
                    redis_fast_path.put(scope, &own_base_url).await;
                }
                if dht_commands.send(put_command(scope, &own_base_url, RECORD_TTL)).await.is_err() {
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

    #[test]
    fn redis_key_never_collides_between_channel_and_conversation() {
        let id = Uuid::new_v4();
        assert_ne!(
            InterestScope::Channel(id).redis_key(),
            InterestScope::Conversation(id).redis_key()
        );
    }

    #[test]
    fn redis_key_is_deterministic_for_the_same_scope() {
        let id = Uuid::new_v4();
        assert_eq!(
            InterestScope::Channel(id).redis_key(),
            InterestScope::Channel(id).redis_key()
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
}
