//! Reference Rust SDK. A game depends on this crate, never on `avalon-server`
//! or `avalon-chain` directly — see `docs/Proposal.md` §17 and
//! `docs/architecture/sdk.md`.
//!
//! `authenticate()` is wired to a real `avalon-server` (GET /me). Everything
//! capability-gated (achievements, guilds, ...) still stubs `NotImplemented`
//! until those endpoints exist — see the epics for each. Friends/presence
//! (issue #17) are the first capability-gated surface that's actually wired
//! to a live endpoint rather than stubbed — see `social`.

pub mod social;

use avalon_protocol::achievements::AchievementAttestation;
use avalon_protocol::identity::{Identity, Profile};
use avalon_protocol::ids::IdentityId;
use avalon_protocol::permissions::Capability;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("capability not granted: {0}")]
    CapabilityNotGranted(String),
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("request to avalon-server failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("avalon-server returned {0}")]
    ServerError(reqwest::StatusCode),
    #[error("not yet implemented")]
    NotImplemented,
}

pub struct AvalonConfig {
    pub server_url: String,
    pub game_credential_key_id: String,
}

pub struct AvalonClient {
    config: AvalonConfig,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct MeResponse {
    identity_id: uuid::Uuid,
    #[serde(with = "time::serde::rfc3339")]
    identity_created_at: time::OffsetDateTime,
    display_name: String,
    avatar_url: Option<String>,
}

impl AvalonClient {
    pub fn new(config: AvalonConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Exchanges a player's existing Avalon session token (obtained via the
    /// Hub or a direct login, not by this SDK — a game never creates
    /// identities itself) for a `Session` scoped to this game.
    pub async fn authenticate(&self, player_token: &str) -> Result<Session, SdkError> {
        let response = self
            .http
            .get(format!("{}/me", self.config.server_url))
            .bearer_auth(player_token)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(SdkError::AuthenticationFailed);
        }

        let body: MeResponse = response.json().await?;

        Ok(Session {
            identity: Identity {
                id: IdentityId(body.identity_id),
                created_at: body.identity_created_at,
            },
            profile: Profile {
                identity_id: IdentityId(body.identity_id),
                display_name: body.display_name,
                avatar_url: body.avatar_url,
            },
            // Permission grants aren't implemented yet (Epic: Game
            // Registration & Permissions) — every capability-gated method
            // correctly rejects until that lands, rather than silently
            // allowing everything.
            granted: Vec::new(),
            http: self.http.clone(),
            server_url: self.config.server_url.clone(),
            token: player_token.to_string(),
        })
    }
}

/// An authenticated player session scoped to whichever capabilities were
/// actually granted — see `Proposal.md` §13. Every read/write method checks
/// its own required capability rather than trusting the caller.
pub struct Session {
    identity: Identity,
    profile: Profile,
    granted: Vec<Capability>,
    http: reqwest::Client,
    server_url: String,
    /// The player's own session bearer token, kept so `Session` methods can
    /// call `avalon-server` on the player's behalf (e.g. `social::friends`,
    /// `social::update_presence`) without the caller having to thread it
    /// through again.
    token: String,
}

impl Session {
    fn require(&self, capability: &str) -> Result<(), SdkError> {
        if self.granted.iter().any(|c| c.0 == capability) {
            Ok(())
        } else {
            Err(SdkError::CapabilityNotGranted(capability.to_string()))
        }
    }

    /// Test-only escape hatch: `authenticate()` always returns a `Session`
    /// with no grants (the capability-grant system, #26–#28, isn't built
    /// yet), so integration tests that need to exercise a capability-gated
    /// method against a real server have no other way to get one. Delete
    /// this once `authenticate()` can populate `granted` from the server.
    ///
    /// Gated behind the `test-util` feature (on for this crate's own dev
    /// builds, off otherwise) rather than merely `#[doc(hidden)]` — the
    /// server doesn't enforce capability grants yet either (#28), so a
    /// `pub` method that self-grants capabilities would otherwise ship as
    /// a real, callable capability bypass in every game's build.
    #[cfg(feature = "test-util")]
    pub fn grant_for_testing(mut self, capability: &str) -> Self {
        self.granted.push(Capability::new(capability));
        self
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub async fn achievements(&self) -> Result<Vec<AchievementAttestation>, SdkError> {
        self.require("achievements.read")?;
        Err(SdkError::NotImplemented)
    }

    pub async fn issue_achievement(&self, _achievement: &str) -> Result<(), SdkError> {
        self.require("achievements.issue")?;
        Err(SdkError::NotImplemented)
    }
}
