//! Push notification for mirror sync — the delivery half of
//! push-based mirror sync, built entirely on top of `crate::interest`'s
//! already-existing DHT-backed registration rather than
//! a new addressing mechanism. See `crate::interest`'s own module doc for
//! why `InterestScope::Network` reuses that exact registration/lookup
//! path instead of inventing a second one.
//!
//! **Delivery transport: direct HTTP, not a libp2p stream.** Once
//! [`notify_peers`] resolves the interested set via `interest::lookup`,
//! it POSTs a small notification straight to each peer's known HTTP base
//! URL (the same `base_url` identity `crate::nodes`' peer table and
//! `mirror_watcher`'s own polling already use) rather than opening a
//! direct libp2p stream. Chosen because every piece an HTTP push needs —
//! a `reqwest::Client`, the peer's reachable base URL, an axum route to
//! receive it — already exists in this codebase for the exact same shape
//! of problem, while a libp2p stream would require adding a brand-new
//! request-response protocol to `crate::dht`'s swarm (today only
//! Kademlia put/get plus `identify` — see that module's own doc comment)
//! for a one-shot, fire-and-forget notification that never needs a
//! persistent bidirectional connection. The DHT is still what makes the
//! *addressing* targeted rather than a broadcast; only the last hop is
//! plain HTTP.
//!
//! **Never trusted content.** A receiving node's `POST /mirror/notify`
//! handler ([`notify`]) does nothing with the reported `network_id`/
//! `tree_size` beyond logging — it exists purely to wake
//! `mirror_watcher::run_worker`'s loop early so it re-polls (and
//! re-corroborates, per #300/#316's still-unweakened gate) every
//! configured peer for that network right away, instead of waiting out
//! the rest of the current poll interval. Every actual verification step
//! is exactly `mirror_watcher`'s existing signature/inclusion-proof
//! pipeline, run on this node's own initiative against its own
//! configured peers — a push notification is a prompt to look, never
//! something looked at directly.
//!
//! **Additive to tier 1.** A node with `AVALON_DHT_ENABLED` unset (or one
//! nobody has registered interest for) gets no pushes at all and mirrors
//! exactly as it always has, on poll alone — see
//! `mirror_watcher`'s own module doc for the two-tier design this
//! implements.

use serde::{Deserialize, Serialize};

use crate::dht::DhtCommandSender;
use crate::interest::{self, InterestScope, RedisFastPath};
use crate::state::AppState;

/// Bundles what [`notify_peers`] needs to resolve interested peers and
/// reach them — built once at startup (`main.rs`) alongside the other
/// DHT-dependent workers. Never constructed at all when
/// `AVALON_DHT_ENABLED` is unset (`dht_commands` has nothing to hand it),
/// in which case `main.rs` simply never spawns anything that would call
/// [`notify_peers`] — tier 1 (permissionless polling) is completely
/// unaffected either way, per this ticket's own invariant.
#[derive(Clone)]
pub struct MirrorPushConfig {
    dht_commands: DhtCommandSender,
    redis_fast_path: Option<RedisFastPath>,
    own_base_url: Option<String>,
    client: reqwest::Client,
}

impl MirrorPushConfig {
    pub fn new(
        dht_commands: DhtCommandSender,
        redis_fast_path: Option<RedisFastPath>,
        own_base_url: Option<String>,
    ) -> Self {
        Self {
            dht_commands,
            redis_fast_path,
            own_base_url,
            client: crate::outbound_policy::peer_client(),
        }
    }
}

/// The wire body for `POST /mirror/notify` — deliberately just enough to
/// let a receiving node log what prompted the wake-up and decide whether
/// it's even worth re-polling early (a `tree_size` at or below what this
/// node already has would still just fall through to its normal
/// verify/corroborate pass, never trusted directly). See this module's own
/// doc comment for why nothing here is ever trusted as settled fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorNotifyRequest {
    pub network_id: String,
    pub tree_size: i64,
}

/// Looks up every peer currently registered as interested in `network_id`
/// (via `crate::interest::lookup`, reusing #585's Redis fast-path when
/// configured) and POSTs a best-effort [`MirrorNotifyRequest`] to each,
/// excluding this node's own `own_base_url` (a node that both authors and
/// mirrors the same network would otherwise notify itself). Every
/// individual delivery is fire-and-forget and logged, never propagated as
/// an error — a peer that's unreachable or slow to answer is no worse off
/// than it already was pre-#596: it just falls back to its own poll tick,
/// which is exactly the correctness backstop this design leans on.
pub async fn notify_peers(config: &MirrorPushConfig, network_id: &str, tree_size: i64) {
    let scope = InterestScope::for_network(network_id);
    let peers =
        interest::lookup(&config.dht_commands, scope, config.redis_fast_path.as_ref()).await;
    if peers.is_empty() {
        return;
    }

    let body = MirrorNotifyRequest {
        network_id: network_id.to_string(),
        tree_size,
    };
    for peer in peers {
        if config.own_base_url.as_deref() == Some(peer.as_str()) {
            continue;
        }
        let client = config.client.clone();
        let peer_url = peer.clone();
        let body = body.clone();
        tokio::spawn(async move {
            match client
                .post(format!("{peer_url}/mirror/notify"))
                .json(&body)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    tracing::debug!(
                        peer = %peer_url,
                        network_id = %body.network_id,
                        tree_size = body.tree_size,
                        "mirror-push: notified peer of a new STH"
                    );
                }
                Ok(response) => {
                    tracing::warn!(
                        peer = %peer_url,
                        status = %response.status(),
                        "mirror-push: peer rejected push notification — its own poll fallback still applies"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        peer = %peer_url,
                        error = %err,
                        "mirror-push: failed to reach peer — its own poll fallback still applies"
                    );
                }
            }
        });
    }
}

/// `POST /mirror/notify` — see this module's own doc comment for the full
/// "never trusted content" invariant. Always `202 Accepted`, even for a
/// node not currently mirroring anything at all (a stray or
/// misconfigured push from an unrelated peer is harmless noise, not an
/// error) — `state.mirror_wake.notify_one()` is a no-op with nobody
/// listening.
pub async fn notify(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(body): axum::Json<MirrorNotifyRequest>,
) -> axum::http::StatusCode {
    tracing::info!(
        network_id = %body.network_id,
        tree_size = body.tree_size,
        "mirror-push: received a push notification — waking the mirror-watcher early"
    );
    state.mirror_wake.notify_one();
    axum::http::StatusCode::ACCEPTED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_request_round_trips_through_json() {
        let body = MirrorNotifyRequest {
            network_id: "avalon-dev-local".to_string(),
            tree_size: 42,
        };
        let json = serde_json::to_string(&body).expect("serialize");
        let decoded: MirrorNotifyRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.network_id, body.network_id);
        assert_eq!(decoded.tree_size, body.tree_size);
    }
}
