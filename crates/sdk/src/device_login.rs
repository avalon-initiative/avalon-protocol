//! `AvalonClient::login()` (issue #398) — the ergonomic wrapper around
//! #307's cross-device pairing (`crates/server/src/device_pairing.rs`) that
//! #307 itself left as optional SDK scope. Without this, every integrator
//! with no WebAuthn surface (a game engine, a console, a headless client)
//! would hand-sequence `POST /auth/device/start` → its own poll loop
//! against `POST /auth/device/poll` → `AvalonClient::authenticate()`
//! itself, each one probably getting the backoff/expiry handling slightly
//! wrong. This module is that loop, written once.
//!
//! `login()` returns a [`DeviceLogin`] carrying `user_code`/
//! `verification_uri` — everything needed to show the user a code or QR —
//! without the SDK picking a QR-rendering library on the caller's behalf;
//! how to display those two strings is left entirely to the integrator
//! (a Unity texture, a console dashboard, a terminal). [`DeviceLogin::wait`]
//! then drives the poll loop to completion and resolves to a real
//! [`Session`] — the same type `authenticate()` returns, since an approved
//! pairing mints an ordinary session token (see
//! `crates/server/src/device_pairing.rs`'s module docs) that this method
//! feeds straight into `AvalonClient::authenticate`, not a second,
//! differently-shaped result type.

use std::time::Duration;

use serde::Deserialize;

use crate::{AvalonClient, SdkError, Session};

/// The minimum gap `POST /auth/device/poll` allows between polls before
/// answering `slow_down`, per `crates/server/src/device_pairing.rs`'s own
/// `POLL_MIN_INTERVAL_SECONDS`. Used only as a floor if a server response
/// ever omits `poll_interval`; the server's own value is preferred whenever
/// present.
const DEFAULT_POLL_INTERVAL_SECONDS: i64 = 5;
/// Cap on the backoff `wait()` applies after repeated `slow_down`
/// responses, so a long-pending pairing doesn't end up polling once a
/// minute.
const MAX_POLL_INTERVAL_SECONDS: i64 = 60;

#[derive(Deserialize)]
struct StartPairingResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    poll_interval: i64,
}

#[derive(Deserialize)]
struct PollPairingResponse {
    status: String,
    token: Option<String>,
}

/// A pending cross-device pairing, returned by [`AvalonClient::login`].
/// Borrows the `AvalonClient` it was started from, the same way
/// `Session::submission_transport` borrows its `Session` — a pairing has no
/// reason to outlive the client that started it.
pub struct DeviceLogin<'a> {
    client: &'a AvalonClient,
    device_code: String,
    /// Short, human-typeable code to show the user — render it as text, or
    /// embed `verification_uri` in a QR code, whichever fits the calling
    /// context.
    pub user_code: String,
    /// Where the user completes approval on a WebAuthn-capable device
    /// (typically the Hub). Already has `user_code` embedded as a query
    /// param — see `crates/server/src/device_pairing.rs::hub_verification_uri`.
    pub verification_uri: String,
    /// Seconds until this pairing expires if it's never approved.
    pub expires_in: i64,
    poll_interval: i64,
}

impl AvalonClient {
    /// Starts a cross-device pairing (#307) via `POST /auth/device/start`.
    /// Use this instead of `authenticate()` when this process has no
    /// WebAuthn ceremony surface of its own — `authenticate()` still needs
    /// a session token obtained some other way, and an approved
    /// [`DeviceLogin::wait`] is one way to get one.
    pub async fn login(&self) -> Result<DeviceLogin<'_>, SdkError> {
        let response = self
            .http
            .post(format!("{}/auth/device/start", self.config.server_url))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(SdkError::ServerError(response.status()));
        }
        let body: StartPairingResponse = response.json().await?;

        Ok(DeviceLogin {
            client: self,
            device_code: body.device_code,
            user_code: body.user_code,
            verification_uri: body.verification_uri,
            expires_in: body.expires_in,
            poll_interval: if body.poll_interval > 0 {
                body.poll_interval
            } else {
                DEFAULT_POLL_INTERVAL_SECONDS
            },
        })
    }
}

impl DeviceLogin<'_> {
    /// Drives `POST /auth/device/poll` to completion. Sleeps
    /// `poll_interval` seconds between polls, doubling that interval (up to
    /// [`MAX_POLL_INTERVAL_SECONDS`]) whenever the server answers
    /// `slow_down` — the same backoff shape the standard OAuth
    /// device-authorization grant uses — rather than treating `slow_down`
    /// as just another `pending`. Resolves to a real [`Session`] on
    /// `approved` (exchanging the minted token via
    /// `AvalonClient::authenticate`, the identical path a normal WebAuthn
    /// login already uses), or a typed [`SdkError`] on `denied`/`expired`.
    pub async fn wait(&self) -> Result<Session, SdkError> {
        let mut interval = self.poll_interval.max(1);

        loop {
            tokio::time::sleep(Duration::from_secs(interval as u64)).await;

            let response = self
                .client
                .http
                .post(format!(
                    "{}/auth/device/poll",
                    self.client.config.server_url
                ))
                .bearer_auth(&self.device_code)
                .send()
                .await?;
            if !response.status().is_success() {
                return Err(SdkError::ServerError(response.status()));
            }
            let body: PollPairingResponse = response.json().await?;

            match body.status.as_str() {
                "pending" => continue,
                "slow_down" => {
                    interval = (interval * 2).min(MAX_POLL_INTERVAL_SECONDS);
                    continue;
                }
                "denied" => return Err(SdkError::DeviceLoginDenied),
                "approved" => {
                    // Single-use delivery (`device_pairing.rs`'s own
                    // invariant): a missing token here would mean this
                    // response already lost a consumption race, which the
                    // server surfaces as `expired`, not `approved` with no
                    // token — treat it the same as a malformed response.
                    let token = body.token.ok_or(SdkError::AuthenticationFailed)?;
                    return self.client.authenticate(&token).await;
                }
                // "expired" and anything unrecognized both mean this
                // pairing is done and will never resolve to a session.
                _ => return Err(SdkError::DeviceLoginExpired),
            }
        }
    }
}
