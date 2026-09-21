//! `AccountSession` — a first-party client for an identity's own account
//! (issue #699, on top of #696's decision and #697/#698's action-tier
//! classification/enforcement), entirely distinct from the
//! capability-gated integrator [`crate::Session`] `crate::AvalonClient::authenticate`
//! already builds. There is deliberately no `From`/`Into` between the two
//! and no `AccountSession` constructor that accepts an integrator
//! credential anywhere in its signature — #696's hard invariant that an
//! integrator credential must never yield account-level power holds at
//! the type level, not just by convention.
//!
//! Obtained one of three ways, all on [`crate::AvalonClient`]:
//! [`AvalonClient::register`] (a brand-new identity, driving a real
//! WebAuthn registration ceremony against a virtual authenticator — see
//! `webauthn` submodule), [`AvalonClient::account_login`] (an existing
//! identity logging back in from credentials [`AvalonClient::register`]
//! returned earlier), or [`AvalonClient::resume_account_session`] /
//! [`AvalonClient::resume_account_session_with_signing_key`] (an
//! already-minted bearer token, e.g. one Hub itself stored — mirrors how
//! Hub resumes a persisted session).
//!
//! Every action #697 flags as signature-required is signed automatically
//! here using whatever local Ed25519 signing key this session holds —
//! see [`AccountSession::sign`]. A session built via
//! [`AvalonClient::resume_account_session`] (token only, no local key)
//! simply sends those actions unsigned, same as the Hub frontend does
//! when it has no local signing key for this device yet; the server's own
//! `NO_REGISTERED_SIGNING_KEY`/`FRESH_SIGNATURE_REQUIRED` split
//! (`crates/server/src/signature_gate.rs`) surfaces the real problem
//! rather than this crate inventing new client-side handling for it.
//!
//! Submodules split by concern, mirroring `guilds.rs`/`social.rs`'s
//! existing "impl blocks on the session type, grouped by domain, one file
//! per domain" convention for [`crate::Session`] — just for
//! [`AccountSession`] instead.

mod webauthn;

pub mod conversations;
pub mod devices;
pub mod guild_admin;
pub mod integrations;
pub mod passkeys;
pub mod recovery;
pub mod social;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use avalon_protocol::identity::{Identity, Profile};
use avalon_protocol::ids::{GuildId, IdentityId};

use crate::{AvalonClient, RetryConfig, SdkError};

/// Builds the canonical `avalon:<action_tag>:v1:<field1>:<field2>:...`
/// byte string a signature-required action signs — must match
/// `signature_gate::canonical_message` in
/// `crates/server/src/signature_gate.rs` byte-for-byte (see this module's
/// own unit tests below).
pub(crate) fn canonical_message(action_tag: &str, fields: &[&str]) -> Vec<u8> {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    message.into_bytes()
}

/// The `signing_key_id`/`signature` pair every signature-required request
/// carries alongside its own fields — flattened into that request's own
/// `Serialize` body via `#[serde(flatten)]` at each call site. Both `None`
/// (serializing as explicit JSON `null`s, which every signature-required
/// handler's `Option<Uuid>`/`Option<String>` fields already accept) when
/// this session holds no local signing key.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct SignatureFields {
    pub(crate) signing_key_id: Option<Uuid>,
    pub(crate) signature: Option<String>,
}

/// This session's own locally-held Ed25519 signing key, plus the
/// server-side `identity_signing_keys.id` it resolves to — the exact pair
/// [`SignatureFields`] needs. Resolved once, at construction time (see
/// [`find_own_signing_key_id`]), not re-fetched on every signed call.
struct AccountSigningKey {
    signing_key: SigningKey,
    signing_key_id: Uuid,
}

/// Local credential material needed to log back into an already-registered
/// identity from this same device (i.e. the same virtual authenticator
/// state) via [`AvalonClient::account_login`] — returned by
/// [`AvalonClient::register`]. Not needed at all for
/// [`AvalonClient::resume_account_session`], which only needs an
/// already-minted bearer token.
///
/// `Serialize`/`Deserialize` so a caller (a CLI, a test harness) can
/// persist this between process runs the same way
/// `crates/cli/src/dev_tools.rs::create_identity` persists its own
/// passkey/signing-key files — this crate never writes to disk itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountCredentials {
    /// The identity these credentials log back into.
    pub identity_id: Uuid,
    /// Base64-encoded 32-byte Ed25519 signing-key seed — the same key
    /// `identity_created_signing_bytes` was signed with at registration.
    pub signing_key_seed_base64: String,
    /// The virtual authenticator's persisted passkey — opaque to callers,
    /// only ever round-tripped back through [`AvalonClient::account_login`].
    pub(crate) passkey: webauthn::StoredPasskey,
}

/// A first-party session for an identity's own account (issue #699) — see
/// this module's own doc comment for how one is obtained and how signing
/// works. Distinct from [`crate::Session`]: no shared fields, no
/// conversion, no capability-grant model (an account's own actions are
/// gated by what the account itself is allowed to do, not by an
/// integrator's granted capabilities).
pub struct AccountSession {
    identity: Identity,
    profile: Profile,
    http: reqwest::Client,
    server_url: String,
    token: String,
    signing: Option<AccountSigningKey>,
    local_credentials: Option<AccountCredentials>,
    retry: RetryConfig,
}

#[derive(Deserialize)]
struct MeResponse {
    identity_id: Uuid,
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
    main_guild: Option<Uuid>,
}

impl From<MeResponse> for (Identity, Profile) {
    fn from(body: MeResponse) -> Self {
        let id = IdentityId(body.identity_id);
        (
            Identity {
                id,
                created_at: body.identity_created_at,
            },
            Profile {
                identity_id: id,
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
        )
    }
}

async fn fetch_me(
    http: &reqwest::Client,
    retry: &RetryConfig,
    server_url: &str,
    token: &str,
) -> Result<(Identity, Profile), SdkError> {
    let response = crate::http::send(http, retry, true, |c| {
        c.get(format!("{server_url}/me")).bearer_auth(token)
    })
    .await?;
    if !response.status().is_success() {
        return Err(crate::http::map_error_response(response).await);
    }
    let body: MeResponse = response
        .json()
        .await
        .map_err(|e| SdkError::Protocol(e.to_string()))?;
    Ok(body.into())
}

#[derive(Deserialize)]
struct DeviceRow {
    id: Uuid,
    public_key: String,
}

/// `GET /me/devices`, matched by base64 public key — the only way a
/// device that only knows its own local secret key learns which
/// server-side `identity_signing_keys` row *is* itself (see
/// `packages/api-client/src/crypto/signingKey.ts::publicKeyFromSecretKey`'s
/// own doc comment for the same lookup, client-side, in the Hub). Returns
/// `None` rather than erroring when no row matches — e.g. a signing key
/// this process holds that was never actually registered server-side.
async fn find_own_signing_key_id(
    http: &reqwest::Client,
    retry: &RetryConfig,
    server_url: &str,
    token: &str,
    public_key_b64: &str,
) -> Result<Option<Uuid>, SdkError> {
    let response = crate::http::send(http, retry, true, |c| {
        c.get(format!("{server_url}/me/devices")).bearer_auth(token)
    })
    .await?;
    if !response.status().is_success() {
        return Err(crate::http::map_error_response(response).await);
    }
    let devices: Vec<DeviceRow> = response
        .json()
        .await
        .map_err(|e| SdkError::Protocol(e.to_string()))?;
    Ok(devices
        .into_iter()
        .find(|d| d.public_key == public_key_b64)
        .map(|d| d.id))
}

#[derive(Serialize)]
struct UpdateProfileRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    favorite_genres: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pronouns: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    banner_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    theme_color: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<&'a str>,
}

/// A partial update to an identity's own profile — every field `None`
/// means "leave untouched," matching `PATCH /me`'s own partial-update
/// convention (`crates/server/src/handlers.rs::update_profile`). Passing
/// `Some("")` on a three-state field (`bio`, `avatar_url`, etc.) clears it.
#[derive(Debug, Clone, Default)]
pub struct ProfileUpdate<'a> {
    /// New display name (globally-unique handle) — `None` leaves it
    /// untouched.
    pub display_name: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub avatar_url: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub bio: Option<&'a str>,
    /// `None` leaves it untouched; `Some(vec![])` clears the list. Each
    /// entry must parse as a [`avalon_protocol::identity::Genre`] or the
    /// server rejects the whole request.
    pub favorite_genres: Option<Vec<String>>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub pronouns: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub banner_url: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub status: Option<&'a str>,
    /// `None` leaves it untouched; `Some(vec![])` clears the list.
    pub links: Option<Vec<String>>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub timezone: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub theme_color: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub location: Option<&'a str>,
}

impl AccountSession {
    /// This session's own identity (id and creation time), as of whenever
    /// this session was built or last refreshed — not re-fetched
    /// automatically.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// This session's own profile, as of whenever this session was built
    /// or last refreshed via [`AccountSession::refresh_profile`].
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The session's own bearer token — for a caller (e.g. a CLI, a test
    /// harness) that wants to persist it and later call
    /// [`AvalonClient::resume_account_session`] instead of registering or
    /// logging in again.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The `identity_signing_keys.id` this session's local signing key
    /// resolves to server-side, if this session holds one at all — `None`
    /// for a session built via [`AvalonClient::resume_account_session`]
    /// with no signing key supplied, in which case every
    /// signature-required method below sends its request unsigned (see
    /// this module's own doc comment).
    pub fn signing_key_id(&self) -> Option<Uuid> {
        self.signing.as_ref().map(|s| s.signing_key_id)
    }

    /// The local credential material [`AvalonClient::account_login`] needs
    /// to log back into this identity from this same device later —
    /// `Some` only for a session built via [`AvalonClient::register`] or
    /// [`AvalonClient::account_login`] itself (both of which drove a real
    /// WebAuthn ceremony and so hold the passkey material), `None` for one
    /// built via [`AvalonClient::resume_account_session`].
    pub fn credentials(&self) -> Option<&AccountCredentials> {
        self.local_credentials.as_ref()
    }

    /// Re-fetches `GET /me` and updates [`AccountSession::profile`] in
    /// place — a caller that just called
    /// [`AccountSession::update_profile`] doesn't need this (the response
    /// already reflects the new state), but one that changed the profile
    /// through another client (e.g. the Hub) does.
    pub async fn refresh_profile(&mut self) -> Result<(), SdkError> {
        let (identity, profile) =
            fetch_me(&self.http, &self.retry, &self.server_url, &self.token).await?;
        self.identity = identity;
        self.profile = profile;
        Ok(())
    }

    /// `PATCH /me` — not signature-required (#697: "profile fields are
    /// self-description, not security state"). Updates
    /// [`AccountSession::profile`] in place from the response on success.
    pub async fn update_profile(&mut self, update: ProfileUpdate<'_>) -> Result<(), SdkError> {
        let body = UpdateProfileRequest {
            display_name: update.display_name,
            avatar_url: update.avatar_url,
            bio: update.bio,
            favorite_genres: update.favorite_genres,
            pronouns: update.pronouns,
            banner_url: update.banner_url,
            status: update.status,
            links: update.links,
            timezone: update.timezone,
            theme_color: update.theme_color,
            location: update.location,
        };
        let me: MeResponse = self.patch("/me", &body).await?;
        let (identity, profile) = me.into();
        self.identity = identity;
        self.profile = profile;
        Ok(())
    }

    /// Signs `action_tag`/`fields` with this session's local key, if it
    /// has one — see [`SignatureFields`]'s own doc comment. Every
    /// signature-required method in the sibling submodules calls this
    /// unconditionally (never a caller-supplied signature), including for
    /// the conditionally-signed endpoints (last-passkey revoke, guardian
    /// removal/threshold-raise, escalating member-role change): an
    /// unused-but-valid signature is harmless, matching the same
    /// simplification the Hub frontend already made for #698.
    fn sign(&self, action_tag: &str, fields: &[&str]) -> SignatureFields {
        match &self.signing {
            Some(signing) => {
                let message = canonical_message(action_tag, fields);
                let signature = signing.signing_key.sign(&message);
                SignatureFields {
                    signing_key_id: Some(signing.signing_key_id),
                    signature: Some(BASE64.encode(signature.to_bytes())),
                }
            }
            None => SignatureFields::default(),
        }
    }

    /// Signs `message` directly with this session's local key — for the
    /// one call site (`devices::approve_device_grant`) whose signed bytes
    /// predate #698's generalized `avalon:<tag>:v1:...` shape enough that
    /// it's clearer to build them explicitly rather than force-fit
    /// [`AccountSession::sign`]'s `(action_tag, fields)` shape (even
    /// though the bytes end up identical — see that call site). Panics if
    /// this session holds no local signing key; callers must check
    /// [`AccountSession::signing_key_id`] first.
    pub(crate) fn sign_raw(&self, message: &[u8]) -> String {
        let signing = self
            .signing
            .as_ref()
            .expect("sign_raw called without a local signing key");
        let signature = signing.signing_key.sign(message);
        BASE64.encode(signature.to_bytes())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.server_url, path)
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, true, |c| {
            c.get(self.url(path)).bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    pub(crate) async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, true, |c| {
            c.get(self.url(path)).bearer_auth(&self.token).query(query)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(self.url(path)).bearer_auth(&self.token).json(body)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    /// A `POST` with no request body at all (e.g. `POST /guilds/{id}/join`).
    pub(crate) async fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(self.url(path)).bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    /// A `POST` carrying a JSON body whose handler returns `Result<(), AppError>`
    /// (axum serializes that as a 200 with an *empty* body, not a JSON
    /// `null` — same generic-empty-body handling
    /// `packages/api-client/src/client.ts::request` does), so the response
    /// body is discarded rather than deserialized.
    pub(crate) async fn post_no_response<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(self.url(path)).bearer_auth(&self.token).json(body)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        Ok(())
    }

    /// Same as [`AccountSession::post_no_response`], but with no request
    /// body at all (e.g. `POST /guilds/{id}/leave`).
    pub(crate) async fn post_empty_no_response(&self, path: &str) -> Result<(), SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(self.url(path)).bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        Ok(())
    }

    pub(crate) async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.patch(self.url(path)).bearer_auth(&self.token).json(body)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    pub(crate) async fn put<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.put(self.url(path)).bearer_auth(&self.token).json(body)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))
    }

    /// A `DELETE` carrying no request body, discarding the response body.
    pub(crate) async fn delete(&self, path: &str) -> Result<(), SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.delete(self.url(path)).bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        Ok(())
    }

    /// A `DELETE` carrying a JSON body (three signature-required endpoints
    /// — see `docs/architecture/identity.md`'s #698 section — take a small
    /// body on what used to be a bodyless `DELETE`), discarding the
    /// response body.
    pub(crate) async fn delete_with_body<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), SdkError> {
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.delete(self.url(path)).bearer_auth(&self.token).json(body)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        Ok(())
    }
}

impl AvalonClient {
    /// Registers a brand-new identity and returns a fresh
    /// [`AccountSession`] for it — issue #699. Drives a real WebAuthn
    /// registration ceremony against a virtual (software-only)
    /// authenticator (see the `account::webauthn` submodule), generates a
    /// fresh Ed25519 event-signing key locally, then immediately logs the
    /// new identity in (`POST /sessions/start`/`finish`, using the same
    /// virtual authenticator) so the returned session has a real bearer
    /// token — `POST /identities/register/finish` itself returns only the
    /// new `identity_id`, not a token.
    ///
    /// [`AccountSession::credentials`] on the result carries what
    /// [`AvalonClient::account_login`] needs to log back into this same
    /// identity later; persist it if this process won't outlive this
    /// session.
    pub async fn register(&self, display_name: &str) -> Result<AccountSession, SdkError> {
        let identity_id = Uuid::new_v4();
        let base = &self.config.server_url;
        let http = &self.http;

        let mut csprng = rand::rng();
        let signing_key = SigningKey::generate(&mut csprng);
        let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

        #[derive(Serialize)]
        struct RegisterStartRequest<'a> {
            identity_id: Uuid,
            display_name: &'a str,
        }
        #[derive(Deserialize)]
        struct RegisterStartResponse {
            ticket_id: Uuid,
            challenge: passkey_types::webauthn::CredentialCreationOptions,
        }
        let start: RegisterStartResponse = http
            .post(format!("{base}/identities/register/start"))
            .json(&RegisterStartRequest {
                identity_id,
                display_name,
            })
            .send()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        let (webauthn_credential, stored_passkey) =
            webauthn::registration_ceremony(start.challenge).await?;

        let signing_bytes = webauthn::identity_created_signing_bytes(identity_id, display_name);
        let event_signature = BASE64.encode(signing_key.sign(&signing_bytes).to_bytes());

        #[derive(Serialize)]
        struct RegisterFinishRequest {
            ticket_id: Uuid,
            webauthn_credential: passkey_types::webauthn::CreatedPublicKeyCredential,
            event_signing_public_key: String,
            event_signature: String,
            device_label: Option<String>,
        }
        let finish_response = http
            .post(format!("{base}/identities/register/finish"))
            .json(&RegisterFinishRequest {
                ticket_id: start.ticket_id,
                webauthn_credential,
                event_signing_public_key,
                event_signature,
                device_label: None,
            })
            .send()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        if !finish_response.status().is_success() {
            return Err(crate::http::map_error_response(finish_response).await);
        }

        let credentials = AccountCredentials {
            identity_id,
            signing_key_seed_base64: BASE64.encode(signing_key.to_bytes()),
            passkey: stored_passkey,
        };

        self.finish_login(identity_id, signing_key, credentials.passkey.clone())
            .await
            .map(|mut session| {
                session.local_credentials = Some(credentials);
                session
            })
    }

    /// Logs back into an identity previously registered via
    /// [`AvalonClient::register`] (on this same virtual-authenticator
    /// state), driving a real `POST /sessions/start`/`finish` WebAuthn
    /// ceremony — issue #699. `credentials` is what
    /// [`AccountSession::credentials`] returned from that earlier
    /// registration (or a prior `account_login`).
    pub async fn account_login(
        &self,
        credentials: &AccountCredentials,
    ) -> Result<AccountSession, SdkError> {
        let seed: [u8; 32] = BASE64
            .decode(&credentials.signing_key_seed_base64)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                SdkError::Protocol(
                    "AccountCredentials::signing_key_seed_base64 was not a valid base64 32-byte seed"
                        .to_string(),
                )
            })?;
        let signing_key = SigningKey::from_bytes(&seed);

        let mut session = self
            .finish_login(
                credentials.identity_id,
                signing_key,
                credentials.passkey.clone(),
            )
            .await?;
        session.local_credentials = Some(credentials.clone());
        Ok(session)
    }

    /// Shared `POST /sessions/start` -> ceremony -> `POST /sessions/finish`
    /// -> `GET /me` -> resolve own `signing_key_id` sequence used by both
    /// [`AvalonClient::register`] (right after `register/finish`) and
    /// [`AvalonClient::account_login`].
    async fn finish_login(
        &self,
        identity_id: Uuid,
        signing_key: SigningKey,
        stored_passkey: webauthn::StoredPasskey,
    ) -> Result<AccountSession, SdkError> {
        let base = &self.config.server_url;
        let http = &self.http;

        #[derive(Serialize)]
        struct SessionStartRequest {
            identity_id: Uuid,
        }
        #[derive(Deserialize)]
        struct SessionStartResponse {
            ticket_id: Uuid,
            challenge: passkey_types::webauthn::CredentialRequestOptions,
        }
        let start: SessionStartResponse = http
            .post(format!("{base}/sessions/start"))
            .json(&SessionStartRequest { identity_id })
            .send()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        let assertion = webauthn::login_ceremony(start.challenge, stored_passkey).await?;

        #[derive(Serialize)]
        struct SessionFinishRequest {
            ticket_id: Uuid,
            credential: passkey_types::webauthn::AuthenticatedPublicKeyCredential,
        }
        #[derive(Deserialize)]
        struct SessionFinishResponse {
            token: String,
        }
        let finish_response = http
            .post(format!("{base}/sessions/finish"))
            .json(&SessionFinishRequest {
                ticket_id: start.ticket_id,
                credential: assertion,
            })
            .send()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        if !finish_response.status().is_success() {
            return Err(crate::http::map_error_response(finish_response).await);
        }
        let finish: SessionFinishResponse = finish_response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        let (identity, profile) = fetch_me(http, &self.config.retry, base, &finish.token).await?;

        let public_key_b64 = BASE64.encode(signing_key.verifying_key().to_bytes());
        let signing_key_id = find_own_signing_key_id(
            http,
            &self.config.retry,
            base,
            &finish.token,
            &public_key_b64,
        )
        .await?;

        Ok(AccountSession {
            identity,
            profile,
            http: http.clone(),
            server_url: base.clone(),
            token: finish.token,
            signing: signing_key_id.map(|signing_key_id| AccountSigningKey {
                signing_key,
                signing_key_id,
            }),
            local_credentials: None,
            retry: self.config.retry.clone(),
        })
    }

    /// Resumes an already-minted bearer token as an [`AccountSession`] —
    /// issue #699, mirroring how Hub resumes a persisted session. Holds no
    /// local signing key, so every signature-required method on the
    /// result sends its request unsigned (see [`AccountSession`]'s own
    /// doc comment) — use
    /// [`AvalonClient::resume_account_session_with_signing_key`] instead
    /// when this process also holds the identity's signing key.
    pub async fn resume_account_session(&self, token: &str) -> Result<AccountSession, SdkError> {
        let (identity, profile) = fetch_me(
            &self.http,
            &self.config.retry,
            &self.config.server_url,
            token,
        )
        .await?;
        Ok(AccountSession {
            identity,
            profile,
            http: self.http.clone(),
            server_url: self.config.server_url.clone(),
            token: token.to_string(),
            signing: None,
            local_credentials: None,
            retry: self.config.retry.clone(),
        })
    }

    /// Same as [`AvalonClient::resume_account_session`], but also resolves
    /// `signing_key_seed`'s server-side `signing_key_id` (via
    /// `GET /me/devices`, matching on public key) so the returned
    /// session's signature-required methods sign automatically — for a
    /// caller that persisted both a bearer token and this identity's
    /// signing-key seed itself (the same two pieces of state Hub's own
    /// `localStorage` keeps, just not through this crate).
    pub async fn resume_account_session_with_signing_key(
        &self,
        token: &str,
        signing_key_seed: [u8; 32],
    ) -> Result<AccountSession, SdkError> {
        let mut session = self.resume_account_session(token).await?;
        let signing_key = SigningKey::from_bytes(&signing_key_seed);
        let public_key_b64 = BASE64.encode(signing_key.verifying_key().to_bytes());
        let signing_key_id = find_own_signing_key_id(
            &self.http,
            &self.config.retry,
            &self.config.server_url,
            token,
            &public_key_b64,
        )
        .await?;
        session.signing = signing_key_id.map(|signing_key_id| AccountSigningKey {
            signing_key,
            signing_key_id,
        });
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_message_matches_server_shape() {
        let message = canonical_message("guild.transfer_ownership", &["g1", "from1", "to1"]);
        assert_eq!(
            String::from_utf8(message).unwrap(),
            "avalon:guild.transfer_ownership:v1:g1:from1:to1"
        );
    }

    #[test]
    fn canonical_message_with_no_fields_is_just_the_tag() {
        let message = canonical_message("integration.connect", &[]);
        assert_eq!(
            String::from_utf8(message).unwrap(),
            "avalon:integration.connect:v1"
        );
    }

    #[test]
    fn sign_with_no_local_key_yields_empty_signature_fields() {
        let session = AccountSession {
            identity: Identity {
                id: IdentityId(Uuid::new_v4()),
                created_at: time::OffsetDateTime::now_utc(),
            },
            profile: Profile {
                identity_id: IdentityId(Uuid::new_v4()),
                display_name: "test".to_string(),
                avatar_url: None,
                bio: None,
                favorite_genres: Vec::new(),
                pronouns: None,
                banner_url: None,
                status: None,
                links: Vec::new(),
                timezone: None,
                theme_color: None,
                location: None,
                main_guild: None,
            },
            http: reqwest::Client::new(),
            server_url: "http://127.0.0.1:1".to_string(),
            token: "test-token".to_string(),
            signing: None,
            local_credentials: None,
            retry: RetryConfig::default(),
        };
        let signed = session.sign("guild.transfer_ownership", &["g1", "from1", "to1"]);
        assert!(signed.signing_key_id.is_none());
        assert!(signed.signature.is_none());
    }

    #[test]
    fn sign_with_a_local_key_produces_a_verifiable_signature() {
        let mut csprng = rand::rng();
        let signing_key = SigningKey::generate(&mut csprng);
        let signing_key_id = Uuid::new_v4();

        let session = AccountSession {
            identity: Identity {
                id: IdentityId(Uuid::new_v4()),
                created_at: time::OffsetDateTime::now_utc(),
            },
            profile: Profile {
                identity_id: IdentityId(Uuid::new_v4()),
                display_name: "test".to_string(),
                avatar_url: None,
                bio: None,
                favorite_genres: Vec::new(),
                pronouns: None,
                banner_url: None,
                status: None,
                links: Vec::new(),
                timezone: None,
                theme_color: None,
                location: None,
                main_guild: None,
            },
            http: reqwest::Client::new(),
            server_url: "http://127.0.0.1:1".to_string(),
            token: "test-token".to_string(),
            signing: Some(AccountSigningKey {
                signing_key: signing_key.clone(),
                signing_key_id,
            }),
            local_credentials: None,
            retry: RetryConfig::default(),
        };

        let signed = session.sign("passkey.revoke_last", &["p1", "i1"]);
        assert_eq!(signed.signing_key_id, Some(signing_key_id));
        let signature_bytes: [u8; 64] = BASE64
            .decode(signed.signature.unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let signature = ed25519_dalek::Signature::from_bytes(&signature_bytes);
        use ed25519_dalek::Verifier;
        assert!(signing_key
            .verifying_key()
            .verify(
                &canonical_message("passkey.revoke_last", &["p1", "i1"]),
                &signature
            )
            .is_ok());
    }
}
