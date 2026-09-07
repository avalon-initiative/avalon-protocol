//! Reference Rust SDK. A game depends on this crate, never on `avalon-server`
//! or `avalon-chain` directly — see `Proposal.md` §17 and `PROMPT.md` §18-19.
//!
//! This is scaffolding: the shapes below are the intended public surface: a
//! game creates a client, authenticates a player, requests capabilities, then
//! reads/writes only what those capabilities allow. None of it talks to a
//! real `avalon-server` yet.

use avalon_protocol::achievements::AchievementAttestation;
use avalon_protocol::identity::Identity;
use avalon_protocol::permissions::Capability;

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("capability not granted: {0}")]
    CapabilityNotGranted(String),
    #[error("not yet implemented")]
    NotImplemented,
}

pub struct AvalonConfig {
    pub server_url: String,
    pub game_credential_key_id: String,
}

pub struct AvalonClient {
    #[allow(dead_code)]
    config: AvalonConfig,
}

impl AvalonClient {
    pub fn new(config: AvalonConfig) -> Self {
        Self { config }
    }

    pub async fn authenticate(&self, _player_token: &str) -> Result<Session, SdkError> {
        Err(SdkError::NotImplemented)
    }
}

/// An authenticated player session scoped to whichever capabilities were
/// actually granted — see `Proposal.md` §13. Every read/write method checks
/// its own required capability rather than trusting the caller.
pub struct Session {
    identity: Identity,
    granted: Vec<Capability>,
}

impl Session {
    fn require(&self, capability: &str) -> Result<(), SdkError> {
        if self.granted.iter().any(|c| c.0 == capability) {
            Ok(())
        } else {
            Err(SdkError::CapabilityNotGranted(capability.to_string()))
        }
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
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
