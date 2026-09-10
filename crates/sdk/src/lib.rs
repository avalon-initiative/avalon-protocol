//! Reference Rust SDK. An integrator (game, app, or service) depends on this
//! crate, never on `avalon-server` or `avalon-chain` directly — see
//! `docs/stakeholders/Proposal.md` §17 and `docs/architecture/sdk.md`.
//!
//! `authenticate()` is wired to a real `avalon-server`: `GET /me` for
//! identity/profile, `GET /me/grants` (issue #27) for this integrator's own
//! active capability grants for the authenticating identity, identified via
//! `AvalonConfig::game_credential_key_id`. Achievements still stub
//! `NotImplemented` until that epic's endpoints exist. Friends/presence
//! (issue #17, see `social`) and guild membership/roster/channels/chat
//! (issue #23, see `guilds`) are wired to live endpoints rather than
//! stubbed. `sync_journal` (issue #110) is the local durable-storage half
//! of offline participation described in
//! `docs/architecture/synchronization.md`; `submission` (issue #111) is the
//! drain/retry/reconciliation half. Neither is wired into `AvalonClient`'s
//! or `Session`'s other methods yet — no method appends to the journal on
//! its own. An integrator constructs a `FileJournal` and `SubmissionEngine`
//! itself and drives them explicitly; `Session::submission_transport`
//! supplies the `Transport` the engine submits through.

pub mod conversations;
pub mod guilds;
pub mod social;
pub mod submission;
pub mod sync_journal;

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
    #[error("presence websocket connection failed: {0}")]
    WebSocket(String),
    /// The server rejected a conversation read or send with "not a
    /// participant" (`crates/server/src/conversations.rs::require_unblocked_participant`).
    /// The server deliberately returns this identical error whether the
    /// caller was never a participant *or* is a blocked one (issue #97:
    /// "never reveal you've been blocked, not even indirectly") — this
    /// variant carries nothing beyond that fact on purpose. Do not add a
    /// field to it that would let a caller distinguish the two cases; doing
    /// so would defeat the server-side protection this type is mirroring.
    #[error("not a participant in this conversation")]
    NotConversationParticipant,
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
    bio: Option<String>,
    favorite_genres: Vec<avalon_protocol::identity::Genre>,
    pronouns: Option<String>,
}

impl AvalonClient {
    pub fn new(config: AvalonConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Exchanges an identity's existing Avalon session token (obtained via
    /// the Hub or a direct login, not by this SDK — an integrator never
    /// creates identities itself) for a `Session` scoped to this integrator.
    pub async fn authenticate(&self, identity_token: &str) -> Result<Session, SdkError> {
        let response = self
            .http
            .get(format!("{}/me", self.config.server_url))
            .bearer_auth(identity_token)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(SdkError::AuthenticationFailed);
        }

        let body: MeResponse = response.json().await?;

        // Permission grants (#27) — `GET /me/grants` returns this
        // integrator's own active grants for the authenticating identity,
        // identified via `game_credential_key_id`
        // (`x-avalon-integrator-key-id` — #293's generic header name; the
        // server still accepts the older `x-avalon-game-key-id` too, but
        // this SDK sends only the new one). A non-success response (e.g. an
        // unrecognized/placeholder key id, or the integrator has no grants
        // yet) is treated as "no grants" rather than an authentication
        // failure — the identity token already proved who they are; an
        // unknown integrator key just means this integrator has nothing
        // granted, same as if it had never connected.
        let granted = self.fetch_granted(identity_token).await.unwrap_or_default();

        Ok(Session {
            identity: Identity {
                id: IdentityId(body.identity_id),
                created_at: body.identity_created_at,
            },
            profile: Profile {
                identity_id: IdentityId(body.identity_id),
                display_name: body.display_name,
                avatar_url: body.avatar_url,
                bio: body.bio,
                favorite_genres: body.favorite_genres,
                pronouns: body.pronouns,
            },
            granted,
            http: self.http.clone(),
            server_url: self.config.server_url.clone(),
            token: identity_token.to_string(),
        })
    }

    async fn fetch_granted(&self, identity_token: &str) -> Result<Vec<Capability>, SdkError> {
        let response = self
            .http
            .get(format!("{}/me/grants", self.config.server_url))
            .bearer_auth(identity_token)
            .header(
                "x-avalon-integrator-key-id",
                &self.config.game_credential_key_id,
            )
            .send()
            .await?;

        if !response.status().is_success() {
            return Ok(Vec::new());
        }

        let body: MyGrantsResponse = response.json().await?;
        Ok(body
            .capabilities
            .into_iter()
            .map(Capability::from)
            .collect())
    }
}

#[derive(Deserialize)]
struct MyGrantsResponse {
    capabilities: Vec<String>,
}

/// An authenticated identity session scoped to whichever capabilities were
/// actually granted — see `Proposal.md` §13. Every read/write method checks
/// its own required capability rather than trusting the caller.
pub struct Session {
    identity: Identity,
    profile: Profile,
    granted: Vec<Capability>,
    http: reqwest::Client,
    server_url: String,
    /// The identity's own session bearer token, kept so `Session` methods can
    /// call `avalon-server` on the player's behalf (e.g. `social::friends`,
    /// `social::update_presence`) without the caller having to thread it
    /// through again.
    token: String,
}

impl Session {
    /// Takes the enum, not a bare string (#98) — every capability-gated
    /// method call site (`social.rs`, below) references a `Capability`
    /// variant, never a string literal.
    fn require(&self, capability: Capability) -> Result<(), SdkError> {
        if self.granted.contains(&capability) {
            Ok(())
        } else {
            Err(SdkError::CapabilityNotGranted(
                capability.as_str().to_string(),
            ))
        }
    }

    /// Test-only escape hatch, kept even now that `authenticate()`
    /// populates `granted` from a real `GET /me/grants` call (#27): using
    /// the real flow end to end means driving a full integrator registration +
    /// identity consent (`POST /games/{slug}/connect`) for every test that
    /// needs a granted capability, which `crates/sdk/tests/guilds.rs` and
    /// `crates/sdk/tests/social.rs` do not otherwise need to exercise —
    /// they're testing `guilds.rs`/`social.rs`'s methods, not the consent
    /// flow itself (that's `crates/server/tests/connections.rs`'s job).
    /// This escape hatch still lets them get a granted `Session` directly.
    ///
    /// Gated behind the `test-util` feature (on for this crate's own dev
    /// builds, off otherwise) rather than merely `#[doc(hidden)]` — the
    /// server doesn't enforce capability grants yet either (#28), so a
    /// `pub` method that self-grants capabilities would otherwise ship as
    /// a real, callable capability bypass in every integrator's build.
    ///
    /// Takes `impl Into<Capability>` rather than `Capability` directly so
    /// existing test call sites can keep passing a raw string
    /// (`.grant_for_testing("presence.read")`) — an unrecognized string
    /// still round-trips through `Capability::Other` per #98, it just
    /// isn't the primary way non-test code is meant to reach for this.
    #[cfg(feature = "test-util")]
    pub fn grant_for_testing(mut self, capability: impl Into<Capability>) -> Self {
        self.granted.push(capability.into());
        self
    }

    /// Builds the [`crate::submission::HttpTransport`] a
    /// [`crate::submission::SubmissionEngine`] submits through, borrowing
    /// this session — an integrator never constructs `HttpTransport` directly. Not
    /// capability-gated itself: each operation kind the transport actually
    /// submits enforces whatever the underlying `Session`/handle method
    /// already enforces (e.g. `messages.send`, conversation
    /// participation/blocking, per `crates/sdk/src/conversations.rs` and
    /// `crates/server/src/conversations.rs`).
    ///
    /// Scoped to `&self`: a session whose token has expired needs a fresh
    /// `AvalonClient::authenticate()` call (a new `Session`) before
    /// draining again — see `crates/sdk/src/submission.rs`'s
    /// `SubmitError::AuthenticationRequired` doc comment.
    pub fn submission_transport(&self) -> crate::submission::HttpTransport<'_> {
        crate::submission::HttpTransport::new(self)
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub async fn achievements(&self) -> Result<Vec<AchievementAttestation>, SdkError> {
        self.require(Capability::AchievementsRead)?;
        Err(SdkError::NotImplemented)
    }

    pub async fn issue_achievement(&self, _achievement: &str) -> Result<(), SdkError> {
        self.require(Capability::AchievementsIssue)?;
        Err(SdkError::NotImplemented)
    }
}
