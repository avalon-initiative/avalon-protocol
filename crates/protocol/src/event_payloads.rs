//! Typed `ProtocolEvent::payload` shapes, one per [`crate::events::ProtocolEventKindVariant`]
//! (issue #82) — the emitter builds one of these and serializes it via
//! `serde_json::to_value`, instead of hand-typing an ad-hoc
//! `serde_json::json!({...})` at each call site. `docs/architecture/protocol-events-catalogue.md`
//! is the normative, human-readable description of the same shapes.
//!
//! Deliberately **not** `#[serde(deny_unknown_fields)]` on any of these —
//! per the versioning policy (`docs/architecture/protocol-events.md`), an
//! additive field never bumps `version`, so a decoder must silently ignore
//! a field it doesn't yet know about rather than erroring on it.
//!
//! **One deliberate exception, issue #510**: `discriminator` was removed
//! from `IdentityCreatedPayload`/`ProfileUpdatedPayload` outright, not
//! added-alongside-and-deprecated the way the versioning policy otherwise
//! requires for a field removal (which would normally bump `version` and
//! keep the old shape decodable forever). Same grounds
//! `docs/architecture/nodes.md`'s #290 exception already documents: this
//! repo has no real deployed network and zero external integrators yet,
//! so there is no real historical data anywhere that needs the old shape
//! to stay decodable. Once the repo is public this exception is gone
//! permanently — a future field removal needs its own decision issue, not
//! a citation of #510 as precedent.
//!
//! A double-`Option` field (`Option<Option<T>>`) is this repo's existing
//! "clear vs. untouched" convention (`crate::guilds::UpdateGuildRequest::motd`
//! and friends): the outer `None` means the key is entirely absent from the
//! payload (untouched), `Some(None)` serializes as an explicit JSON `null`
//! (cleared), and `Some(Some(v))` serializes as `v` (set). Every field below
//! using it is `#[serde(skip_serializing_if = "Option::is_none")]` so the
//! outer `None` case is a genuinely absent key, not a redundant `null`.

use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::guilds::GuildLink;

// --- identity.* -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityCreatedPayload {
    pub identity_id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentitySigningKeyAddedPayload {
    pub signing_key_id: Uuid,
    pub public_key: String,
    pub device_label: Option<String>,
    pub approved_by_signing_key_id: Uuid,
    /// Issue #525 — the identity this key belongs to. Every emitter already
    /// knows it (it's `issuer`/`subject`'s own owner segment), but
    /// `GlobalId` has no public accessor to pull it back out of an
    /// already-built `ProtocolEvent`, so a decoder that needs it
    /// (`avalon_indexer::projections::identity_signing_keys`) needs it
    /// carried explicitly, same as `IdentityCreatedPayload`/
    /// `IdentityPasskeyRegisteredPayload` already do. A historical event
    /// emitted before this field existed simply won't decode into that
    /// (also brand-new) projection — harmless, since `identity_signing_keys`
    /// itself, this node's own local source of truth, was never missing it.
    pub identity_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentitySigningKeyRevokedPayload {
    pub signing_key_id: Uuid,
}

/// Issue #523: a WebAuthn passkey's *public* credential material only —
/// never anything secret, since a platform authenticator never gives the
/// server (or, by extension, replayed ledger history) anything but this in
/// the first place. `passkey_data` is `webauthn-rs`'s own serialized
/// `Passkey` (COSE public key, signature counter, transports, backup
/// state) — the exact JSON `identity_keys.passkey_data` already stores,
/// carried through unchanged so a mirror-only node can reconstruct a
/// verification-ready credential from replayed history alone, not just an
/// opaque blob. `credential_id` is base64-encoded, same convention
/// `IdentitySigningKeyAddedPayload::public_key` already uses for raw bytes
/// in a JSON payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityPasskeyRegisteredPayload {
    pub passkey_id: Uuid,
    pub identity_id: Uuid,
    pub credential_id: String,
    pub passkey_data: serde_json::Value,
    pub label: Option<String>,
}

/// Issue #523: network-attributed, same milestone-1 precedent
/// `IdentitySigningKeyRevokedPayload`/`friend.requested` already use —
/// revocation only ever narrows trust, so it doesn't need a higher signing
/// bar than registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityPasskeyRevokedPayload {
    pub passkey_id: Uuid,
    pub identity_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecoveryConfiguredPayload {
    pub guardian_ids: Vec<Uuid>,
    pub threshold: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecoveryRequestedPayload {
    pub request_id: Uuid,
    pub threshold: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecoveryApprovedPayload {
    pub request_id: Uuid,
    pub guardian_id: Uuid,
    pub approvals_count: i64,
    pub threshold: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub delay_ends_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecoveryCancelledPayload {
    pub request_id: Uuid,
    pub cancelled_by: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecoveredPayload {
    pub request_id: Uuid,
    pub device_label: Option<String>,
}

// --- profile.updated --------------------------------------------------------
//
// Sparse by design (issue #86, widened by #155): only the keys that actually
// changed are present at all. Shared by `handlers.rs::update_profile` and
// `guilds.rs::leave_guild` (which clears `main_guild`).

/// Serde's derived `Deserialize` for `Option<T>` treats a present JSON
/// `null` the same as an absent key (both become `None`) — which silently
/// collapses this struct's whole "absent = untouched, `null` = cleared"
/// distinction on the read side, even though `skip_serializing_if` alone
/// gets the *write* side right. This is the standard workaround (every
/// `Option<Option<T>>` field below pairs it with `default` so a genuinely
/// absent key still deserializes to the outer `None`): wrap the inner
/// value in `Some` unconditionally, since `deserialize_with` only runs at
/// all when the key is present.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileUpdatedPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub avatar_url: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub bio: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite_genres: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub pronouns: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub banner_url: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub status: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub timezone: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub theme_color: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub location: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some"
    )]
    pub main_guild: Option<Option<Uuid>>,
}

// --- game.* / permission.* --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameRegisteredKeyPayload {
    pub key_id: Uuid,
    pub algorithm: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameRegisteredPayload {
    pub game_id: Uuid,
    pub slug: String,
    pub name: String,
    pub developer: String,
    pub category: String,
    pub requested_capabilities: Vec<String>,
    pub initial_key: GameRegisteredKeyPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameBindingEstablishedPayload {
    pub binding_id: Uuid,
    pub identity_id: Uuid,
    pub game_id: Uuid,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameBindingEndedPayload {
    pub binding_id: Uuid,
    pub identity_id: Uuid,
    pub game_id: Uuid,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionGrantedPayload {
    pub binding_id: Uuid,
    pub identity_id: Uuid,
    pub game_id: Uuid,
    pub capability: String,
}

/// Shared by `connections::revoke_grant` (no `reason`) and
/// `connections::disconnect` (`reason: "binding_ended"`) — unified into one
/// optional field rather than two payload shapes under the same kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRevokedPayload {
    pub binding_id: Uuid,
    pub identity_id: Uuid,
    pub game_id: Uuid,
    pub capability: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// --- issuer.* ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssuerRegisteredPayload {
    pub issuer_ref: String,
    pub network_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
}

fn default_key_purpose() -> String {
    crate::integrators::KeyPurpose::Attestation
        .as_str()
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssuerKeyAddedPayload {
    pub game_id: Uuid,
    pub slug: String,
    pub key_id: Uuid,
    pub algorithm: String,
    pub public_key: String,
    pub role: String,
    /// Issue #543. `#[serde(default = "default_key_purpose")]` (not a
    /// bare `String` default, which would be `""`) so every
    /// `issuer.key_added` event recorded before #543 existed — real
    /// historical ledger entries — still decodes as exactly what it
    /// already meant: `"attestation"`.
    #[serde(default = "default_key_purpose")]
    pub purpose: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option",
        default
    )]
    pub valid_until: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssuerKeyRevokedPayload {
    pub game_id: Uuid,
    pub slug: String,
    pub key_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub revoked_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// --- friend.* ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendRequestedPayload {
    pub from: Uuid,
    pub to: Uuid,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendAcceptedPayload {
    pub from: Uuid,
    pub to: Uuid,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendRemovedPayload {
    pub a: Uuid,
    pub b: Uuid,
    pub actor: Uuid,
}

// --- guild.* ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildCreatedPayload {
    pub guild_id: Uuid,
    pub name: String,
    pub tag: String,
    pub description: String,
    pub owner: Uuid,
}

/// Full-replace, same convention `guilds::UpdateGuildRequest` itself uses —
/// every field is always present, not sparse like `ProfileUpdatedPayload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildUpdatedPayload {
    pub guild_id: Uuid,
    pub name: String,
    pub tag: String,
    pub description: String,
    pub motd: Option<String>,
    pub banner: Option<String>,
    pub icon: Option<String>,
    pub links: Vec<GuildLink>,
    pub recruiting: bool,
    pub public: bool,
    pub game_breakdown_public: bool,
    pub join_policy: String,
    pub roster_visibility: String,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildRoleBadgePayload {
    pub icon: String,
    pub color: String,
}

/// Shared by `create_role` and `update_role` — identical shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildRoleDefinedPayload {
    pub guild_id: Uuid,
    pub name_index: i32,
    pub name: String,
    pub permissions: Vec<String>,
    pub description: String,
    pub badge: GuildRoleBadgePayload,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildRoleDeletedPayload {
    pub guild_id: Uuid,
    pub name_index: i32,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildMemberAddedPayload {
    pub guild_id: Uuid,
    pub identity_id: Uuid,
    pub role_index: i32,
    /// e.g. `"invite"` / `"join_request"` / `"direct_join"`.
    pub via: String,
    pub actor: Uuid,
}

/// Shared by `leave_guild` (`reason: "left"`) and `remove_member`
/// (`reason: "removed"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildMemberRemovedPayload {
    pub guild_id: Uuid,
    pub identity_id: Uuid,
    pub reason: String,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildRoleChangedPayload {
    pub guild_id: Uuid,
    pub identity_id: Uuid,
    pub role_index: i32,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildOwnerTransferredPayload {
    pub guild_id: Uuid,
    pub from: Uuid,
    pub to: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildGameAssociatedPayload {
    pub guild_id: Uuid,
    pub game_id: Uuid,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildFavoriteGamesUpdatedPayload {
    pub guild_id: Uuid,
    pub game_ids: Vec<Uuid>,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildChannelCreatedPayload {
    pub guild_id: Uuid,
    pub channel_id: Uuid,
    pub name: String,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildChannelRenamedPayload {
    pub guild_id: Uuid,
    pub channel_id: Uuid,
    pub name: String,
    pub announcement_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Issue #458.
    pub public: bool,
    pub actor: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildChannelArchivedPayload {
    pub guild_id: Uuid,
    pub channel_id: Uuid,
    pub actor: Uuid,
}

// --- game_schema.* / game_schema_mapping.* / game_data.* --------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameSchemaPublishedPayload {
    pub id: String,
    pub game_id: Uuid,
    pub slug: String,
    pub version: u32,
    pub proto_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub default_visibility: String,
    pub field_visibility: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameSchemaMappingPublishedPayload {
    pub id: String,
    pub integrator_id: Uuid,
    pub slug: String,
    pub from_schema_id: String,
    pub to_schema_id: String,
    pub description: String,
    pub field_correspondence: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameDataPublishedPayload {
    pub id: String,
    pub schema: String,
    pub game_id: Uuid,
    pub subject: Uuid,
    pub instance: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

/// Issue #533: append-only tombstone for a published instance (e.g. a
/// deleted character) — `docs/architecture/revocation.md`'s pattern
/// applied to Integrator Space instance data. Never mutates or removes
/// `instance_id`'s original row; a projection marks it deleted from this
/// event forward while the original `game_data.published` event (and this
/// one) both stay observable in raw history. `reason_code`/`reason` match
/// `ClaimRevokedPayload`'s *shape* but deliberately stay a free-text
/// `String`, not #534's `RevocationReasonCode` — see
/// `docs/architecture/revocation.md`'s "Entity/instance deletion" section
/// for why that vocabulary (an issuer's judgment call about validity)
/// doesn't fit a subject's own choice to delete their own data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameDataDeletedPayload {
    pub instance_id: String,
    pub schema: String,
    pub game_id: Uuid,
    pub subject: Uuid,
    pub reason_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// --- achievement.* / milestone.* --------------------------------------------
//
// One shared shape per row, used for both the `achievement.*` and
// `milestone.*` kind (the claim-vocabulary split, #324/#325, is which *kind*
// is chosen, never a payload difference).

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimDefinedPayload {
    pub id: String,
    pub game_id: Uuid,
    pub slug: String,
    pub key: String,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub version: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimDefinitionUpdatedPayload {
    pub id: String,
    pub game_id: Uuid,
    pub slug: String,
    pub key: String,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub version: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimDefinitionRetiredPayload {
    pub id: String,
    pub game_id: Uuid,
    pub slug: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimProofPayload {
    pub key_id: Uuid,
    pub algorithm: String,
    pub bytes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimIssuedPayload {
    pub id: Uuid,
    pub issuer: String,
    pub subject: Uuid,
    pub achievement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    pub proof: ClaimProofPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimRevokedPayload {
    pub id: Uuid,
    pub attestation_id: Uuid,
    pub issuer: String,
    /// Issue #534: a real, extensible vocabulary
    /// (`avalon_protocol::revocation::RevocationReasonCode`), not a
    /// free-text string — still a plain JSON string on the wire, so this
    /// is not a breaking change to the event's stored shape.
    pub reason_code: crate::revocation::RevocationReasonCode,
    pub reason: String,
}

// --- integrator.recognition_* -----------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegratorRecognitionPublishedPayload {
    pub recognizer_id: Uuid,
    pub recognized_id: Uuid,
    pub scope: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegratorRecognitionRevokedPayload {
    pub recognizer_id: Uuid,
    pub recognized_id: Uuid,
}

#[cfg(test)]
mod tests {
    //! One test per payload kind, each doing double duty as both the
    //! "round-trips through serde" test and the "this literal fixture
    //! keeps decoding" test #82 asks for: the JSON literal embedded in
    //! each test *is* a historical fixture for this struct's current
    //! version, and it must never stop decoding once anything real has
    //! shipped referencing it (see this module's own doc comment on the
    //! versioning policy).
    use super::*;

    #[test]
    fn identity_created_round_trips() {
        let payload = IdentityCreatedPayload {
            identity_id: Uuid::nil(),
            display_name: "Aria".to_string(),
        };
        let json = serde_json::json!({
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "display_name": "Aria",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityCreatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_signing_key_added_round_trips() {
        let payload = IdentitySigningKeyAddedPayload {
            signing_key_id: Uuid::nil(),
            public_key: "base64key".to_string(),
            device_label: Some("Pixel 9".to_string()),
            approved_by_signing_key_id: Uuid::nil(),
            identity_id: Uuid::nil(),
        };
        let json = serde_json::json!({
            "signing_key_id": "00000000-0000-0000-0000-000000000000",
            "public_key": "base64key",
            "device_label": "Pixel 9",
            "approved_by_signing_key_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentitySigningKeyAddedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_signing_key_revoked_round_trips() {
        let payload = IdentitySigningKeyRevokedPayload {
            signing_key_id: Uuid::nil(),
        };
        let json = serde_json::json!({ "signing_key_id": "00000000-0000-0000-0000-000000000000" });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentitySigningKeyRevokedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_passkey_registered_round_trips() {
        let payload = IdentityPasskeyRegisteredPayload {
            passkey_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            credential_id: "base64credentialid".to_string(),
            passkey_data: serde_json::json!({"cred": "data"}),
            label: Some("Work laptop".to_string()),
        };
        let json = serde_json::json!({
            "passkey_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "credential_id": "base64credentialid",
            "passkey_data": {"cred": "data"},
            "label": "Work laptop",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityPasskeyRegisteredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_passkey_revoked_round_trips() {
        let payload = IdentityPasskeyRevokedPayload {
            passkey_id: Uuid::nil(),
            identity_id: Uuid::nil(),
        };
        let json = serde_json::json!({
            "passkey_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityPasskeyRevokedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_recovery_configured_round_trips() {
        let payload = IdentityRecoveryConfiguredPayload {
            guardian_ids: vec![Uuid::nil()],
            threshold: 2,
        };
        let json = serde_json::json!({
            "guardian_ids": ["00000000-0000-0000-0000-000000000000"],
            "threshold": 2,
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityRecoveryConfiguredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_recovery_requested_round_trips() {
        let payload = IdentityRecoveryRequestedPayload {
            request_id: Uuid::nil(),
            threshold: 2,
        };
        let json = serde_json::json!({
            "request_id": "00000000-0000-0000-0000-000000000000",
            "threshold": 2,
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityRecoveryRequestedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_recovery_approved_round_trips_with_and_without_delay() {
        let with_delay = IdentityRecoveryApprovedPayload {
            request_id: Uuid::nil(),
            guardian_id: Uuid::nil(),
            approvals_count: 1,
            threshold: 2,
            delay_ends_at: Some(OffsetDateTime::UNIX_EPOCH),
        };
        let json = serde_json::json!({
            "request_id": "00000000-0000-0000-0000-000000000000",
            "guardian_id": "00000000-0000-0000-0000-000000000000",
            "approvals_count": 1,
            "threshold": 2,
            "delay_ends_at": "1970-01-01T00:00:00Z",
        });
        assert_eq!(serde_json::to_value(&with_delay).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityRecoveryApprovedPayload>(json).unwrap(),
            with_delay
        );

        let without_delay = IdentityRecoveryApprovedPayload {
            delay_ends_at: None,
            ..with_delay
        };
        let round_tripped: IdentityRecoveryApprovedPayload =
            serde_json::from_value(serde_json::to_value(&without_delay).unwrap()).unwrap();
        assert_eq!(round_tripped, without_delay);
    }

    #[test]
    fn identity_recovery_cancelled_round_trips() {
        let payload = IdentityRecoveryCancelledPayload {
            request_id: Uuid::nil(),
            cancelled_by: Uuid::nil(),
            reason: Some("owner vetoed".to_string()),
        };
        let json = serde_json::json!({
            "request_id": "00000000-0000-0000-0000-000000000000",
            "cancelled_by": "00000000-0000-0000-0000-000000000000",
            "reason": "owner vetoed",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityRecoveryCancelledPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn identity_recovered_round_trips() {
        let payload = IdentityRecoveredPayload {
            request_id: Uuid::nil(),
            device_label: Some("New Phone".to_string()),
        };
        let json = serde_json::json!({
            "request_id": "00000000-0000-0000-0000-000000000000",
            "device_label": "New Phone",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IdentityRecoveredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn profile_updated_only_serializes_present_keys() {
        let payload = ProfileUpdatedPayload {
            display_name: Some("New Name".to_string()),
            avatar_url: Some(None), // explicit clear
            ..Default::default()
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "display_name": "New Name", "avatar_url": null })
        );
        assert_eq!(
            serde_json::from_value::<ProfileUpdatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn profile_updated_distinguishes_absent_from_explicit_null() {
        let untouched = ProfileUpdatedPayload::default();
        assert_eq!(
            serde_json::to_value(&untouched).unwrap(),
            serde_json::json!({})
        );

        let cleared = ProfileUpdatedPayload {
            bio: Some(None),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            serde_json::json!({ "bio": null })
        );

        let set = ProfileUpdatedPayload {
            bio: Some(Some("hello".to_string())),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            serde_json::json!({ "bio": "hello" })
        );
    }

    #[test]
    fn game_registered_round_trips() {
        let payload = GameRegisteredPayload {
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            name: "Ashen Realms".to_string(),
            developer: "Test Studio".to_string(),
            category: "game".to_string(),
            requested_capabilities: vec!["achievements.issue".to_string()],
            initial_key: GameRegisteredKeyPayload {
                key_id: Uuid::nil(),
                algorithm: "ed25519".to_string(),
                public_key: "base64key".to_string(),
            },
        };
        let json = serde_json::json!({
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "name": "Ashen Realms",
            "developer": "Test Studio",
            "category": "game",
            "requested_capabilities": ["achievements.issue"],
            "initial_key": {
                "key_id": "00000000-0000-0000-0000-000000000000",
                "algorithm": "ed25519",
                "public_key": "base64key",
            },
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GameRegisteredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn game_binding_established_round_trips() {
        let payload = GameBindingEstablishedPayload {
            binding_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
        };
        let json = serde_json::json!({
            "binding_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GameBindingEstablishedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn game_binding_ended_round_trips() {
        let payload = GameBindingEndedPayload {
            binding_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
        };
        let json = serde_json::json!({
            "binding_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GameBindingEndedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn permission_granted_round_trips() {
        let payload = PermissionGrantedPayload {
            binding_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            game_id: Uuid::nil(),
            capability: "achievements.issue".to_string(),
        };
        let json = serde_json::json!({
            "binding_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "capability": "achievements.issue",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<PermissionGrantedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn permission_revoked_round_trips_with_and_without_reason() {
        let with_reason = PermissionRevokedPayload {
            binding_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            game_id: Uuid::nil(),
            capability: "achievements.issue".to_string(),
            reason: Some("binding_ended".to_string()),
        };
        let json = serde_json::json!({
            "binding_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "capability": "achievements.issue",
            "reason": "binding_ended",
        });
        assert_eq!(serde_json::to_value(&with_reason).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<PermissionRevokedPayload>(json).unwrap(),
            with_reason
        );

        let without_reason = PermissionRevokedPayload {
            reason: None,
            ..with_reason
        };
        let json_without = serde_json::to_value(&without_reason).unwrap();
        assert!(json_without.get("reason").is_none());
        assert_eq!(
            serde_json::from_value::<PermissionRevokedPayload>(json_without).unwrap(),
            without_reason
        );
    }

    #[test]
    fn issuer_registered_round_trips() {
        let payload = IssuerRegisteredPayload {
            issuer_ref: "game:ashen-realms".to_string(),
            network_id: "avalon-dev-local".to_string(),
            registered_at: OffsetDateTime::UNIX_EPOCH,
        };
        let json = serde_json::json!({
            "issuer_ref": "game:ashen-realms",
            "network_id": "avalon-dev-local",
            "registered_at": "1970-01-01T00:00:00Z",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IssuerRegisteredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn issuer_key_added_round_trips() {
        let payload = IssuerKeyAddedPayload {
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            key_id: Uuid::nil(),
            algorithm: "ed25519".to_string(),
            public_key: "base64key".to_string(),
            role: "operational".to_string(),
            purpose: "attestation".to_string(),
            valid_until: None,
        };
        let json = serde_json::json!({
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "key_id": "00000000-0000-0000-0000-000000000000",
            "algorithm": "ed25519",
            "public_key": "base64key",
            "role": "operational",
            "purpose": "attestation",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IssuerKeyAddedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn issuer_key_added_with_no_purpose_field_defaults_to_attestation() {
        // Issue #543: real historical `issuer.key_added` events recorded
        // before this field existed have no `purpose` key at all — this
        // must decode as `"attestation"`, exactly what every such key
        // already meant, not an error.
        let pre_543_json = serde_json::json!({
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "key_id": "00000000-0000-0000-0000-000000000000",
            "algorithm": "ed25519",
            "public_key": "base64key",
            "role": "operational",
        });
        let payload: IssuerKeyAddedPayload = serde_json::from_value(pre_543_json).unwrap();
        assert_eq!(payload.purpose, "attestation");
    }

    #[test]
    fn issuer_key_revoked_round_trips() {
        let payload = IssuerKeyRevokedPayload {
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            key_id: Uuid::nil(),
            revoked_at: OffsetDateTime::UNIX_EPOCH,
            reason: Some("rotated".to_string()),
        };
        let json = serde_json::json!({
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "key_id": "00000000-0000-0000-0000-000000000000",
            "revoked_at": "1970-01-01T00:00:00Z",
            "reason": "rotated",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IssuerKeyRevokedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn friend_requested_round_trips() {
        let payload = FriendRequestedPayload {
            from: Uuid::nil(),
            to: Uuid::nil(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "from": "00000000-0000-0000-0000-000000000000",
            "to": "00000000-0000-0000-0000-000000000000",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<FriendRequestedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn friend_accepted_round_trips() {
        let payload = FriendAcceptedPayload {
            from: Uuid::nil(),
            to: Uuid::nil(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "from": "00000000-0000-0000-0000-000000000000",
            "to": "00000000-0000-0000-0000-000000000000",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<FriendAcceptedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn friend_removed_round_trips() {
        let payload = FriendRemovedPayload {
            a: Uuid::nil(),
            b: Uuid::nil(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "a": "00000000-0000-0000-0000-000000000000",
            "b": "00000000-0000-0000-0000-000000000000",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<FriendRemovedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_created_round_trips() {
        let payload = GuildCreatedPayload {
            guild_id: Uuid::nil(),
            name: "Dragon Hunters".to_string(),
            tag: "DRGN".to_string(),
            description: "a guild".to_string(),
            owner: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "name": "Dragon Hunters",
            "tag": "DRGN",
            "description": "a guild",
            "owner": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildCreatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_updated_round_trips() {
        let payload = GuildUpdatedPayload {
            guild_id: Uuid::nil(),
            name: "Dragon Hunters".to_string(),
            tag: "DRGN".to_string(),
            description: "a guild".to_string(),
            motd: None,
            banner: None,
            icon: None,
            links: vec![],
            recruiting: true,
            public: false,
            game_breakdown_public: false,
            join_policy: "open".to_string(),
            roster_visibility: "guild_members".to_string(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "name": "Dragon Hunters",
            "tag": "DRGN",
            "description": "a guild",
            "motd": null,
            "banner": null,
            "icon": null,
            "links": [],
            "recruiting": true,
            "public": false,
            "game_breakdown_public": false,
            "join_policy": "open",
            "roster_visibility": "guild_members",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildUpdatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_role_defined_round_trips() {
        let payload = GuildRoleDefinedPayload {
            guild_id: Uuid::nil(),
            name_index: 2,
            name: "Officer".to_string(),
            permissions: vec!["manage_members".to_string()],
            description: "Keeps order".to_string(),
            badge: GuildRoleBadgePayload {
                icon: "shield".to_string(),
                color: "gold".to_string(),
            },
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "name_index": 2,
            "name": "Officer",
            "permissions": ["manage_members"],
            "description": "Keeps order",
            "badge": { "icon": "shield", "color": "gold" },
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildRoleDefinedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_role_deleted_round_trips() {
        let payload = GuildRoleDeletedPayload {
            guild_id: Uuid::nil(),
            name_index: 3,
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "name_index": 3,
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildRoleDeletedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_member_added_round_trips() {
        let payload = GuildMemberAddedPayload {
            guild_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            role_index: 2,
            via: "invite".to_string(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "role_index": 2,
            "via": "invite",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildMemberAddedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_member_removed_round_trips() {
        let payload = GuildMemberRemovedPayload {
            guild_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            reason: "left".to_string(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "reason": "left",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildMemberRemovedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_role_changed_round_trips() {
        let payload = GuildRoleChangedPayload {
            guild_id: Uuid::nil(),
            identity_id: Uuid::nil(),
            role_index: 1,
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "identity_id": "00000000-0000-0000-0000-000000000000",
            "role_index": 1,
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildRoleChangedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_owner_transferred_round_trips() {
        let payload = GuildOwnerTransferredPayload {
            guild_id: Uuid::nil(),
            from: Uuid::nil(),
            to: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "from": "00000000-0000-0000-0000-000000000000",
            "to": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildOwnerTransferredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_game_associated_round_trips() {
        let payload = GuildGameAssociatedPayload {
            guild_id: Uuid::nil(),
            game_id: Uuid::nil(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildGameAssociatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_favorite_games_updated_round_trips() {
        let payload = GuildFavoriteGamesUpdatedPayload {
            guild_id: Uuid::nil(),
            game_ids: vec![Uuid::nil()],
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "game_ids": ["00000000-0000-0000-0000-000000000000"],
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildFavoriteGamesUpdatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_channel_created_round_trips() {
        let payload = GuildChannelCreatedPayload {
            guild_id: Uuid::nil(),
            channel_id: Uuid::nil(),
            name: "general".to_string(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "channel_id": "00000000-0000-0000-0000-000000000000",
            "name": "general",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildChannelCreatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_channel_renamed_round_trips() {
        let payload = GuildChannelRenamedPayload {
            guild_id: Uuid::nil(),
            channel_id: Uuid::nil(),
            name: "general".to_string(),
            announcement_only: false,
            topic: Some("chat about the game".to_string()),
            public: false,
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "channel_id": "00000000-0000-0000-0000-000000000000",
            "name": "general",
            "announcement_only": false,
            "topic": "chat about the game",
            "public": false,
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildChannelRenamedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn guild_channel_archived_round_trips() {
        let payload = GuildChannelArchivedPayload {
            guild_id: Uuid::nil(),
            channel_id: Uuid::nil(),
            actor: Uuid::nil(),
        };
        let json = serde_json::json!({
            "guild_id": "00000000-0000-0000-0000-000000000000",
            "channel_id": "00000000-0000-0000-0000-000000000000",
            "actor": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GuildChannelArchivedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn game_schema_published_round_trips() {
        let payload = GameSchemaPublishedPayload {
            id: "game:ashen-realms:schema:v1".to_string(),
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            version: 1,
            proto_source: "syntax = \"proto3\";".to_string(),
            supersedes: None,
            default_visibility: "public".to_string(),
            field_visibility: serde_json::json!({}),
        };
        let json = serde_json::json!({
            "id": "game:ashen-realms:schema:v1",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "version": 1,
            "proto_source": "syntax = \"proto3\";",
            "default_visibility": "public",
            "field_visibility": {},
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GameSchemaPublishedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn game_schema_mapping_published_round_trips() {
        let payload = GameSchemaMappingPublishedPayload {
            id: "mapping:1".to_string(),
            integrator_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            from_schema_id: "schema:a".to_string(),
            to_schema_id: "schema:b".to_string(),
            description: "maps a to b".to_string(),
            field_correspondence: serde_json::json!({}),
        };
        let json = serde_json::json!({
            "id": "mapping:1",
            "integrator_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "from_schema_id": "schema:a",
            "to_schema_id": "schema:b",
            "description": "maps a to b",
            "field_correspondence": {},
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GameSchemaMappingPublishedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn game_data_published_round_trips() {
        let payload = GameDataPublishedPayload {
            id: "instance:1".to_string(),
            schema: "schema:a".to_string(),
            game_id: Uuid::nil(),
            subject: Uuid::nil(),
            instance: serde_json::json!({ "level": 5 }),
            supersedes: None,
        };
        let json = serde_json::json!({
            "id": "instance:1",
            "schema": "schema:a",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "subject": "00000000-0000-0000-0000-000000000000",
            "instance": { "level": 5 },
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<GameDataPublishedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn claim_defined_round_trips() {
        let payload = ClaimDefinedPayload {
            id: "game:ashen-realms:achievement:dragon_slayer".to_string(),
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            key: "dragon_slayer".to_string(),
            name: "Dragon Slayer".to_string(),
            description: "Slew the dragon".to_string(),
            schema: None,
            icon: Some("trophy".to_string()),
            icon_url: None,
            version: 1,
        };
        let json = serde_json::json!({
            "id": "game:ashen-realms:achievement:dragon_slayer",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
            "icon": "trophy",
            "version": 1,
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<ClaimDefinedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn claim_definition_updated_round_trips() {
        let payload = ClaimDefinitionUpdatedPayload {
            id: "game:ashen-realms:achievement:dragon_slayer".to_string(),
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            key: "dragon_slayer".to_string(),
            name: "Dragon Slayer".to_string(),
            description: "Slew the dragon, updated".to_string(),
            schema: None,
            icon: None,
            icon_url: None,
            version: 2,
        };
        let json = serde_json::json!({
            "id": "game:ashen-realms:achievement:dragon_slayer",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon, updated",
            "version": 2,
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<ClaimDefinitionUpdatedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn claim_definition_retired_round_trips() {
        let payload = ClaimDefinitionRetiredPayload {
            id: "game:ashen-realms:achievement:dragon_slayer".to_string(),
            game_id: Uuid::nil(),
            slug: "ashen-realms".to_string(),
            key: "dragon_slayer".to_string(),
        };
        let json = serde_json::json!({
            "id": "game:ashen-realms:achievement:dragon_slayer",
            "game_id": "00000000-0000-0000-0000-000000000000",
            "slug": "ashen-realms",
            "key": "dragon_slayer",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<ClaimDefinitionRetiredPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn claim_issued_round_trips() {
        let payload = ClaimIssuedPayload {
            id: Uuid::nil(),
            issuer: "game:ashen-realms".to_string(),
            subject: Uuid::nil(),
            achievement: "game:ashen-realms:achievement:dragon_slayer".to_string(),
            evidence: None,
            proof: ClaimProofPayload {
                key_id: Uuid::nil(),
                algorithm: "ed25519".to_string(),
                bytes: "base64sig".to_string(),
            },
        };
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "issuer": "game:ashen-realms",
            "subject": "00000000-0000-0000-0000-000000000000",
            "achievement": "game:ashen-realms:achievement:dragon_slayer",
            "proof": {
                "key_id": "00000000-0000-0000-0000-000000000000",
                "algorithm": "ed25519",
                "bytes": "base64sig",
            },
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<ClaimIssuedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn claim_revoked_round_trips() {
        // Issue #534: exercised with a pre-#534 free-text reason_code
        // ("cheating_detected" — real historical ledger entries recorded
        // exactly this string) rather than one of the new known wire
        // strings ("cheating"), on purpose — this proves
        // `RevocationReasonCode`'s `Other` fallback keeps old recorded
        // history decoding correctly, not just new writes.
        let payload = ClaimRevokedPayload {
            id: Uuid::nil(),
            attestation_id: Uuid::nil(),
            issuer: "game:ashen-realms".to_string(),
            reason_code: crate::revocation::RevocationReasonCode::Other(
                "cheating_detected".to_string(),
            ),
            reason: "unauthorized tooling".to_string(),
        };
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "attestation_id": "00000000-0000-0000-0000-000000000000",
            "issuer": "game:ashen-realms",
            "reason_code": "cheating_detected",
            "reason": "unauthorized tooling",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<ClaimRevokedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn integrator_recognition_published_round_trips() {
        let payload = IntegratorRecognitionPublishedPayload {
            recognizer_id: Uuid::nil(),
            recognized_id: Uuid::nil(),
            scope: vec!["achievement".to_string()],
        };
        let json = serde_json::json!({
            "recognizer_id": "00000000-0000-0000-0000-000000000000",
            "recognized_id": "00000000-0000-0000-0000-000000000000",
            "scope": ["achievement"],
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IntegratorRecognitionPublishedPayload>(json).unwrap(),
            payload
        );
    }

    #[test]
    fn integrator_recognition_revoked_round_trips() {
        let payload = IntegratorRecognitionRevokedPayload {
            recognizer_id: Uuid::nil(),
            recognized_id: Uuid::nil(),
        };
        let json = serde_json::json!({
            "recognizer_id": "00000000-0000-0000-0000-000000000000",
            "recognized_id": "00000000-0000-0000-0000-000000000000",
        });
        assert_eq!(serde_json::to_value(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<IntegratorRecognitionRevokedPayload>(json).unwrap(),
            payload
        );
    }
}
