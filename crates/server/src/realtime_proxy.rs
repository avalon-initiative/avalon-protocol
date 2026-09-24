//! Gateway-side WebSocket proxying to a remote Realtime node,
//! extracting `crate::presence`/`crate::chat`'s WebSocket service
//! into its own genuinely separate deployable role.
//!
//! **Connection-topology decision: proxy-through-Gateway, not
//! direct-connect.** See `docs/projects/backend-server/architecture/nodes.md` — the short version:
//! a client keeps talking to exactly one node's URL for everything, the
//! same invariant every other role extraction (internal RPC,
//! Indexer) already preserves, and client
//! routing/discovery doesn't have to exist yet for this to work. The
//! tradeoff accepted: every realtime message now crosses one extra
//! network hop (client -> Gateway -> Realtime -> Gateway -> client)
//! instead of terminating directly on Realtime, and Gateway itself has to
//! manage a proxied connection's lifetime — this module is exactly that
//! management, and nothing else.
//!
//! **What this module is not**: it does not decide *whether* to proxy —
//! that's `AppState::realtime_remote_url`, resolved once at startup in
//! `main.rs` from `AVALON_NODE_ROLES`/`AVALON_REALTIME_URL` (see
//! `crate::nodes::role_included`). `presence::presence_ws`/`chat::chat_ws`
//! check that field themselves and call [`proxy_websocket`] instead of
//! their own local `handle_*_socket` when it's `Some` — this module only
//! implements the actual byte-for-byte pumping once that decision has
//! already been made, using the exact same authenticated, already-upgraded
//! client connection either path would have gotten.
//!
//! **Auth stays exactly as it was.** The caller's `?token=` is
//! authenticated locally (against the same Postgres every role in one
//! deployment shares) by `presence_ws`/`chat_ws` *before* upgrading the
//! client's socket at all, same as the non-proxied path — so an invalid
//! token still gets a real 401, never an upgrade followed by an immediate
//! silent close. The same token is then forwarded, unmodified, as the
//! outbound connection's own `?token=` when dialing the remote Realtime
//! node's identical public endpoint — Realtime re-validates it itself
//! (a second, cheap `sessions` lookup against the same database), rather
//! than this module inventing a second, narrower trust mechanism (e.g.
//! `crate::internal_role`'s operator-internal bearer key) for what is,
//! from the client's point of view, still just its own session.
//!
//! **Once connected, this is a dumb pipe.** No message is inspected,
//! parsed, or re-serialized — `presence::PresenceResponse`/`chat`'s wire
//! shapes stay whatever `handle_presence_socket`/`handle_chat_socket`
//! produce on the Realtime side; this module only moves `Text`/`Binary`/
//! `Ping`/`Pong`/`Close` frames between the two sockets. In particular,
//! `crate::interest`'s DHT registration (via `ChatClientMessage`'s signed
//! `claim`) and `crate::chat::ChatServerMessage::NodeInfo`'s `base_url`
//! still originate from, and are answered by, the Realtime node itself —
//! exactly as if the client had dialed it directly — since the handler
//! that produces/consumes them (`handle_chat_socket`) only ever runs
//! there now, never on a Gateway configured this way.
//!
//! **Failure mode: a Realtime restart mid-session.** If the outbound
//! connection to Realtime drops (restart, crash, network blip), this
//! module's `remote_to_client`/`client_to_remote` pumps both end (a
//! `next().await` returning `None`/`Err` breaks that side's loop, and
//! `tokio::join!` only returns once both have), and [`proxy_websocket`]
//! then sends a real `Close` frame to the client before returning — a
//! clean, visible disconnect, never a silent hang. Reconnecting is left to
//! the client, same as any other WebSocket disconnect; see this issue's
//! own PR description for the current state of Hub/hub-app's
//! reconnect behavior against this.

use axum::extract::ws::{CloseFrame, Message as AxumMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Builds the outbound `ws://`/`wss://` URL this node dials to reach the
/// remote Realtime node's own copy of `path` (e.g. `/ws/presence`),
/// carrying the same `token` the client's own connection to *this* node
/// was authenticated with. `base_url` is `AVALON_REALTIME_URL` — an
/// `http`/`https` URL, same convention every other `*_URL` env var in this
/// crate uses (`AVALON_INDEXER_REMOTE_URL`, `AVALON_SETTLEMENT_REMOTE_URL`),
/// translated to its `ws`/`wss` equivalent here rather than asking an
/// operator to configure two different schemes for what is, to them, one
/// node's one address.
///
/// Parse failure here should be unreachable in practice —
/// `main.rs` already validates `AVALON_REALTIME_URL` parses as a URL
/// before ever constructing `AppState::realtime_remote_url` (see
/// `nodes::realtime_mode_from_env`'s own doc comment) — but this returns
/// `Err` rather than panicking, since a malformed value slipping through
/// should fail this one connection attempt, not take the whole process
/// down.
fn build_remote_ws_url(base_url: &str, path: &str, token: &str) -> Result<String, String> {
    let mut url = url::Url::parse(base_url).map_err(|e| format!("invalid remote URL: {e}"))?;
    let ws_scheme = match url.scheme() {
        "https" => "wss",
        _ => "ws",
    };
    url.set_scheme(ws_scheme)
        .map_err(|_| "failed to set ws/wss scheme".to_string())?;
    url.set_path(path);
    url.query_pairs_mut().clear().append_pair("token", token);
    Ok(url.into())
}

/// Dials `remote_base_url`'s own `path` (forwarding `token`) and pumps
/// frames bidirectionally between it and `client_socket` — the client's
/// already-upgraded, already-authenticated connection to *this* node —
/// until either side closes or errors. See this module's own doc comment
/// for the full design and its invariants.
pub async fn proxy_websocket(
    mut client_socket: WebSocket,
    remote_base_url: String,
    path: &'static str,
    token: String,
) {
    let remote_ws_url = match build_remote_ws_url(&remote_base_url, path, &token) {
        Ok(url) => url,
        Err(err) => {
            tracing::error!(remote = %remote_base_url, error = %err, "realtime proxy: could not build remote URL");
            let _ = client_socket.send(AxumMessage::Close(None)).await;
            return;
        }
    };

    let (remote_stream, _) = match tokio_tungstenite::connect_async(&remote_ws_url).await {
        Ok(pair) => pair,
        Err(err) => {
            // Same "clear disconnect, not a silent hang" posture this
            // module's doc comment promises — a client that dialed a
            // Gateway with no reachable Realtime node gets a real close
            // frame, not a connection that sits open and does nothing.
            tracing::warn!(remote = %remote_base_url, error = %err, "realtime proxy: failed to connect to remote Realtime node");
            let _ = client_socket
                .send(AxumMessage::Close(Some(CloseFrame {
                    code: axum::extract::ws::close_code::AWAY,
                    reason: "realtime node unreachable".into(),
                })))
                .await;
            return;
        }
    };

    let (mut remote_write, mut remote_read) = remote_stream.split();
    let (mut client_write, mut client_read) = client_socket.split();

    let client_to_remote = async {
        loop {
            match client_read.next().await {
                Some(Ok(AxumMessage::Text(text))) => {
                    if remote_write
                        .send(WsMessage::Text(text.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(Ok(AxumMessage::Binary(data))) => {
                    if remote_write.send(WsMessage::Binary(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(AxumMessage::Ping(data))) => {
                    if remote_write.send(WsMessage::Ping(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(AxumMessage::Pong(data))) => {
                    if remote_write.send(WsMessage::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(AxumMessage::Close(_))) | None | Some(Err(_)) => break,
            }
        }
        let _ = remote_write.close().await;
    };

    let remote_to_client = async {
        loop {
            match remote_read.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    if client_write
                        .send(AxumMessage::Text(text.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(Ok(WsMessage::Binary(data))) => {
                    if client_write.send(AxumMessage::Binary(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(WsMessage::Ping(data))) => {
                    if client_write.send(AxumMessage::Ping(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(WsMessage::Pong(data))) => {
                    if client_write.send(AxumMessage::Pong(data)).await.is_err() {
                        break;
                    }
                }
                // `Frame` is tungstenite's raw-frame variant, only ever
                // produced when reading with `read_frame` rather than the
                // `Stream`/`next()` API used here — never actually
                // reachable through this path, but matched explicitly
                // (rather than folded into the fallthrough below) so a
                // future tungstenite upgrade that changes that can't
                // silently start treating a real frame as a close.
                Some(Ok(WsMessage::Frame(_))) => {}
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
            }
        }
        // A clean, visible close for the client on every exit path from
        // this loop — including the Realtime node itself going away
        // mid-session (a restart), which is exactly the "clean disconnect,
        // not a silent hang" case this issue's own tests exercise live.
        let _ = client_write.send(AxumMessage::Close(None)).await;
    };

    tokio::join!(client_to_remote, remote_to_client);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_ws_url_from_an_http_base() {
        let url = build_remote_ws_url("http://127.0.0.1:9090", "/ws/presence", "tok123").unwrap();
        assert_eq!(url, "ws://127.0.0.1:9090/ws/presence?token=tok123");
    }

    #[test]
    fn builds_a_wss_url_from_an_https_base() {
        let url = build_remote_ws_url("https://realtime.example", "/ws/messages", "tok").unwrap();
        assert_eq!(url, "wss://realtime.example/ws/messages?token=tok");
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_does_not_double_up() {
        let url = build_remote_ws_url("http://127.0.0.1:9090/", "/ws/presence", "t").unwrap();
        assert_eq!(url, "ws://127.0.0.1:9090/ws/presence?token=t");
    }

    #[test]
    fn a_malformed_base_url_is_an_error_not_a_panic() {
        assert!(build_remote_ws_url("not a url", "/ws/presence", "t").is_err());
    }
}
