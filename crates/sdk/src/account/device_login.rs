//! `AvalonClient::start_account_device_login` (issue #707, filed after
//! #699 shipped `AccountSession` without it) — the `AccountSession`-
//! returning counterpart to [`crate::device_login`]'s existing
//! `AvalonClient::login`/[`crate::Session`] wrapper around #307's
//! cross-device pairing (`crates/server/src/device_pairing.rs`). Without
//! this, a client with no WebAuthn surface of its own (a game engine, a
//! console, a headless client) had no way to originate a first-party
//! account login at all — only [`AvalonClient::resume_account_session`],
//! which needs a token minted somewhere else first. This module closes
//! that gap the same way [`crate::device_login`] already did for the
//! integrator [`crate::Session`].
//!
//! The resulting [`AccountSession`] holds no local signing key (the
//! approving device is a *different* device with its own key — this one
//! never had a WebAuthn ceremony of its own), so every signature-required
//! method on it sends its request unsigned until this device separately
//! requests and gets approved for its own signing key via
//! [`AccountSession::request_device_grant`] — same story
//! [`AvalonClient::resume_account_session`] already documents.

use std::time::Duration;

use crate::{AvalonClient, SdkError};

use super::AccountSession;

const DEFAULT_POLL_INTERVAL_SECONDS: i64 = 5;
const MAX_POLL_INTERVAL_SECONDS: i64 = 60;

/// A pending cross-device pairing for an [`AccountSession`] login,
/// returned by [`AvalonClient::start_account_device_login`] — see this
/// module's own doc comment. Mirrors [`crate::device_login::DeviceLogin`]
/// field-for-field; a separate type because it resolves to
/// [`AccountSession`], not [`crate::Session`].
pub struct AccountDeviceLogin<'a> {
    client: &'a AvalonClient,
    device_code: String,
    /// Short, human-typeable code to show the user — render it as text, or
    /// embed `verification_uri` in a QR code.
    pub user_code: String,
    /// Where the user completes approval on a WebAuthn-capable device
    /// (typically the Hub). Already has `user_code` embedded as a query
    /// param.
    pub verification_uri: String,
    /// Seconds until this pairing expires if it's never approved.
    pub expires_in: i64,
    poll_interval: i64,
}

impl AvalonClient {
    /// Starts a cross-device pairing (#307) via `POST /auth/device/start`,
    /// resolving to an [`AccountSession`] rather than the integrator
    /// [`crate::Session`] [`AvalonClient::login`] resolves to. Use this
    /// instead of [`AvalonClient::register`]/[`AvalonClient::account_login`]
    /// when this process has no WebAuthn ceremony surface of its own.
    pub async fn start_account_device_login(&self) -> Result<AccountDeviceLogin<'_>, SdkError> {
        let response = crate::http::send(&self.http, &self.config.retry, false, |c| {
            c.post(format!("{}/auth/device/start", self.config.server_url))
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let body: crate::generated::StartPairingResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        Ok(AccountDeviceLogin {
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

impl AccountDeviceLogin<'_> {
    /// Drives `POST /auth/device/poll` to completion — same backoff shape
    /// as [`crate::device_login::DeviceLogin::wait`] (doubling on
    /// `slow_down`, capped at [`MAX_POLL_INTERVAL_SECONDS`]). Resolves to a
    /// real [`AccountSession`] on `approved` (via
    /// [`AvalonClient::resume_account_session`] — the minted token is
    /// ordinary, same as any other login path), or a typed [`SdkError`] on
    /// `denied`/`expired`.
    pub async fn wait(&self) -> Result<AccountSession, SdkError> {
        let mut interval = self.poll_interval.max(1);

        loop {
            tokio::time::sleep(Duration::from_secs(interval as u64)).await;

            let response =
                crate::http::send(&self.client.http, &self.client.config.retry, true, |c| {
                    c.post(format!(
                        "{}/auth/device/poll",
                        self.client.config.server_url
                    ))
                    .bearer_auth(&self.device_code)
                })
                .await?;
            if !response.status().is_success() {
                return Err(crate::http::map_error_response(response).await);
            }
            let body: crate::generated::PollPairingResponse = response
                .json()
                .await
                .map_err(|e| SdkError::Protocol(e.to_string()))?;

            match body.status.as_str() {
                "pending" => continue,
                "slow_down" => {
                    interval = (interval * 2).min(MAX_POLL_INTERVAL_SECONDS);
                    continue;
                }
                "denied" => return Err(SdkError::DeviceLoginDenied),
                "approved" => {
                    let token = body.token.ok_or_else(|| {
                        SdkError::Protocol("approved poll response was missing a token".to_string())
                    })?;
                    return self.client.resume_account_session(&token).await;
                }
                _ => return Err(SdkError::DeviceLoginExpired),
            }
        }
    }
}
