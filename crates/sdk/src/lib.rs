//! Reference Rust SDK. An integrator (integrator, app, or service) depends on this
//! crate, never on `avalon-server` or `avalon-chain` directly — see
//! `docs/stakeholders/Proposal.md` §17 and `docs/architecture/sdk.md`.
//!
//! Every public item is documented (issue #49) — `#![deny(missing_docs)]`
//! below enforces that mechanically rather than by convention alone. See
//! `docs/developers/` for task-oriented guides (`getting-started.md`,
//! `achievements.md`, `capabilities.md`, `guilds-and-friends.md`,
//! `errors-and-retries.md`, `local-development.md`) and `crates/sdk/examples/`
//! for runnable, `make check`-compiled examples; this rustdoc is the
//! reference, not a tutorial.
//!
//! `authenticate()` is wired to a real `avalon-server`: `GET /me` for
//! identity/profile, `GET /me/grants` (issue #27) for this integrator's own
//! active capability grants for the authenticating identity, identified via
//! `AvalonConfig::integrator_credential_key_id`. `AvalonClient::login()` (issue
//! #398, see `device_login`) is the other way to end up with a `Session`,
//! for a client with no WebAuthn surface of its own — it wraps #307's
//! cross-device pairing and resolves to the same `Session` `authenticate()`
//! does. Achievements (issue #34, see
//! `achievements`) and friends/presence (issue #17, see `social`) and
//! guild membership/roster/channels/chat (issue #23, see `guilds`) are all
//! wired to live endpoints rather than stubbed. `sync_journal` (issue #110)
//! is the local durable-storage half of offline participation described in
//! `docs/architecture/synchronization.md`; `submission` (issue #111) is the
//! drain/retry/reconciliation half. Neither is wired into `AvalonClient`'s
//! or `Session`'s other methods yet — no method appends to the journal on
//! its own. An integrator constructs a `FileJournal` and `SubmissionEngine`
//! itself and drives them explicitly; `Session::submission_transport`
//! supplies the `Transport` the engine submits through. `network` (issue
//! #482) is the SDK-side counterpart to the Hub's STH-based network
//! pinning (`docs/architecture/network-trust-anchors.md`) —
//! `AvalonClient::verify_network` independently verifies which network a
//! server actually is before an integrator registers an issuer or submits
//! a write, since `network_id` alone carries no cryptographic authority.

#![deny(missing_docs)]

pub mod achievements;
pub mod conversations;
pub mod device_login;
pub mod guilds;
mod http;
pub mod network;
pub mod registry;
pub mod schema;
pub mod social;
pub mod submission;
pub mod sync_journal;

pub use http::RetryConfig;

use avalon_protocol::identity::{Identity, Profile};
use avalon_protocol::ids::{GuildId, IdentityId};
use avalon_protocol::permissions::Capability;
use serde::Deserialize;

/// Every outcome a `Session`/`AvalonClient` method can return talking to a
/// real `avalon-server` (issue #47) — protocol-level, never a raw
/// `reqwest::Error`/`StatusCode` a game would have to know HTTP to
/// interpret. `crate::http` is where every call site actually produces
/// these; see that module's doc comment for the retry/mapping rules
/// behind them.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// The session token itself was rejected (expired, unknown, malformed)
    /// — distinct from [`Self::CapabilityNotGranted`], which means the
    /// token is fine but this integrator hasn't been granted what the
    /// request needs.
    #[error("unauthorized")]
    Unauthorized,
    /// Either a capability grant this session's own `require()` check
    /// found present is nonetheless rejected server-side (a stale/revoked
    /// grant, or `require()` not covering an endpoint's real requirement
    /// yet), or the server's own 403 means something else it doesn't
    /// expose more specifically over this API — see `crate::http`'s doc
    /// comment on why every 403 maps here.
    #[error("capability not granted: {0}")]
    CapabilityNotGranted(String),
    /// A referenced resource doesn't exist — carries the server's own
    /// message text (not a structured resource kind/id; see #47's own
    /// scope note in `docs/architecture/sdk.md`).
    #[error("not found: {0}")]
    NotFound(String),
    /// The request conflicts with existing state (already exists, already
    /// in that state, etc).
    #[error("conflict: {0}")]
    Conflict(String),
    /// The server understood the request and made a final decision not to
    /// apply it — malformed input, a signature that doesn't verify, a
    /// business-rule violation. Never retried; a caller changing nothing
    /// about the request would get this again.
    #[error("rejected: {reason}")]
    Rejected {
        /// The server's own stable `code` (or, failing that, its `error`
        /// message) explaining why — see `crate::http`'s doc comment.
        reason: String,
    },
    /// A connection error, timeout, or 502/503/504 — either this call
    /// wasn't retried (a non-idempotent write with no `Idempotency-Key`)
    /// or it was retried `retried` times and still didn't succeed. A node
    /// hiccup, not this request being wrong; safe to surface as "try again
    /// later," never as a gameplay-level failure.
    #[error("avalon-server unavailable after {retried} retries: {detail}")]
    Unavailable {
        /// How many retry attempts (beyond the first) this call made
        /// before giving up — `0` if it was never eligible to retry at
        /// all (a non-idempotent write).
        retried: u32,
        /// The last attempt's error message or the underlying transport
        /// error's own `Display` text.
        detail: String,
    },
    /// The response didn't parse as the shape this call expected, or the
    /// transport failed in a way that isn't connectivity (see
    /// [`Self::Unavailable`] for that case) — a malformed/unexpected
    /// server response, not a protocol-level outcome the caller can act
    /// on.
    #[error("unexpected response from avalon-server: {0}")]
    Protocol(String),
    /// `Session::subscribe_presence` (#136): the websocket handshake or
    /// connection itself failed — carries the underlying error's message.
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
    /// `Session::issue_achievement` (#34) needs this integrator's own slug
    /// and signing key (`AvalonConfig::integrator_slug`/`signing_key`) to
    /// authenticate the issuing request and sign the attestation locally —
    /// neither is required for a read-only integration, so both are
    /// `Option`s rather than mandatory config, and this is what's returned
    /// when a caller reaches for issuance without having supplied them.
    #[error("this integrator's integrator_slug/signing_key were not configured")]
    MissingIssuerCredentials,
    /// `device_login::DeviceLogin::wait` (#398): the user explicitly denied
    /// the pairing from the approving device (`POST /auth/device/deny`).
    #[error("device pairing was denied")]
    DeviceLoginDenied,
    /// `device_login::DeviceLogin::wait` (#398): the pairing's ~10-minute
    /// TTL (`crates/server/src/device_pairing.rs`) elapsed before it was
    /// approved or denied.
    #[error("device pairing expired before it was approved")]
    DeviceLoginExpired,
}

/// Everything an integrator supplies to construct an [`AvalonClient`] — the
/// server to talk to, this integrator's own credential, and (optionally)
/// what it needs to issue attestations on its own behalf.
pub struct AvalonConfig {
    /// Base URL of the `avalon-server` this client talks to, e.g.
    /// `http://127.0.0.1:8080` for local dev.
    pub server_url: String,
    /// This integrator's own registered credential key id
    /// (`x-avalon-integrator-key-id`) — identifies which integrator's grants
    /// `authenticate()` fetches via `GET /me/grants`.
    pub integrator_credential_key_id: String,
    /// This integrator's own registered slug — required only by methods
    /// that issue attestations on this integrator's own behalf
    /// (`Session::issue_achievement`, #34). `None` for a read-only
    /// integration.
    pub integrator_slug: Option<String>,
    /// This integrator's own 32-byte Ed25519 signing key seed, held only
    /// in this process — the server never sees it, only a detached
    /// signature (#34's design). `None` for a read-only integration;
    /// required by `Session::issue_achievement`.
    pub signing_key: Option<[u8; 32]>,
    /// Retry/backoff/timeout tuning for every request this SDK makes
    /// (issue #47) — `RetryConfig::default()` for sensible out-of-the-box
    /// behavior.
    pub retry: RetryConfig,
}

/// The entry point: construct one with [`AvalonClient::new`], then call
/// [`AvalonClient::authenticate`] (or [`AvalonClient::login`] —
/// `device_login`) to get a [`Session`] scoped to a real identity and its
/// granted capabilities.
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
    banner_url: Option<String>,
    status: Option<String>,
    links: Vec<String>,
    timezone: Option<String>,
    theme_color: Option<String>,
    location: Option<String>,
    main_guild: Option<uuid::Uuid>,
}

impl AvalonClient {
    /// Builds a client from `config`. Does not itself talk to the network —
    /// see [`AvalonClient::authenticate`].
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
        let response = http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/me", self.config.server_url))
                .bearer_auth(identity_token)
        })
        .await?;

        if !response.status().is_success() {
            return Err(http::map_error_response(response).await);
        }

        let body: MeResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        // Permission grants (#27) — `GET /me/grants` returns this
        // integrator's own active grants for the authenticating identity,
        // identified via `integrator_credential_key_id`
        // (`x-avalon-integrator-key-id` — #293's generic header name; the
        // server still accepts the older `x-avalon-integrator-key-id` too, but
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
                banner_url: body.banner_url,
                status: body.status,
                links: body.links,
                timezone: body.timezone,
                theme_color: body.theme_color,
                location: body.location,
                main_guild: body.main_guild.map(GuildId),
            },
            granted,
            http: self.http.clone(),
            server_url: self.config.server_url.clone(),
            token: identity_token.to_string(),
            integrator_key_id: self.config.integrator_credential_key_id.clone(),
            integrator_slug: self.config.integrator_slug.clone(),
            signing_key: self.config.signing_key,
            retry: self.config.retry.clone(),
        })
    }

    async fn fetch_granted(&self, identity_token: &str) -> Result<Vec<Capability>, SdkError> {
        let response = http::send(&self.http, &self.config.retry, true, |c| {
            c.get(format!("{}/me/grants", self.config.server_url))
                .bearer_auth(identity_token)
                .header(
                    "x-avalon-integrator-key-id",
                    &self.config.integrator_credential_key_id,
                )
        })
        .await?;

        if !response.status().is_success() {
            return Ok(Vec::new());
        }

        let body: MyGrantsResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
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
    /// call `avalon-server` on the user's behalf (e.g. `social::friends`,
    /// `social::update_presence`) without the caller having to thread it
    /// through again.
    token: String,
    /// This integrator's own registered key id
    /// (`AvalonConfig::integrator_credential_key_id`) — the same value already
    /// used for `GET /me/grants`, reused by `achievements::issue_achievement`
    /// (#34) as the challenge-response and embedded-proof `key_id`.
    integrator_key_id: String,
    /// See `AvalonConfig::integrator_slug`.
    integrator_slug: Option<String>,
    /// See `AvalonConfig::signing_key`.
    signing_key: Option<[u8; 32]>,
    /// See `AvalonConfig::retry`.
    retry: RetryConfig,
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
    /// identity consent (`POST /integrations/{slug}/connect`) for every test that
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

    /// This session's own identity (id and creation time) — no network
    /// call, populated once by `authenticate()`.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// This session's own profile, as it was when `authenticate()` ran —
    /// not re-fetched automatically after a subsequent profile edit made
    /// through another client (e.g. the Hub).
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// This identity's own attestation history — see `achievements`
    /// module doc comment for why there's no `recognition` field (#34/#33).
    pub async fn achievements(&self) -> Result<Vec<achievements::VerifiedAttestation>, SdkError> {
        self.require(Capability::AchievementsRead)?;
        self.fetch_achievements().await
    }

    /// Issues `key` (an achievement defined by this integrator) to this
    /// session's own identity, signed locally with
    /// `AvalonConfig::signing_key` — see `achievements` module doc comment.
    /// Returns the new attestation's id.
    pub async fn issue_achievement(&self, key: &str) -> Result<uuid::Uuid, SdkError> {
        self.require(Capability::AchievementsIssue)?;
        self.submit_achievement_issuance(key).await
    }
}
