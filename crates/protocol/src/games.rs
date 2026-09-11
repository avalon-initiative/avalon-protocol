//! Game registration — how a game becomes known to Avalon and what it asks for.
//!
//! See `Proposal.md` §18. Registering does not grant any capability by
//! itself; a player must still authorize each capability (`permissions`).
//!
//! Issuer key lifecycle (issue #80, decided; implemented here per #84):
//! see [`KeyRole`] and [`IssuerKey`].

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::GameId;
use crate::permissions::Capability;

/// A game's registration status (issue #26). Grown by #84 to catalogue the
/// full set `docs/architecture/games-and-issuers.md`'s status column always
/// named (`Active`/`Suspended`/`Revoked`/`Deprecated`) — milestone 1 still
/// only ever *sets* `Active`; the network-level authorization model for who
/// can transition an issuer into the other three, and the endpoints that do
/// it, are explicit follow-up work (#84's own scope note), not built here.
/// A growable enum rather than a bare `bool` so those states have somewhere
/// to land later without a wire format change, same shape `JoinPolicy`
/// (`crates/protocol/src/guilds.rs`) already established in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Active,
    Suspended,
    Revoked,
    Deprecated,
}

impl GameStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GameStatus::Active => "active",
            GameStatus::Suspended => "suspended",
            GameStatus::Revoked => "revoked",
            GameStatus::Deprecated => "deprecated",
        }
    }

    pub fn parse(s: &str) -> Option<GameStatus> {
        Some(match s {
            "active" => GameStatus::Active,
            "suspended" => GameStatus::Suspended,
            "revoked" => GameStatus::Revoked,
            "deprecated" => GameStatus::Deprecated,
            _ => return None,
        })
    }
}

/// An issuer key's role (issue #80, decided; implemented here per #84) —
/// the axis that determines what the key is trusted to authorize, entirely
/// independent of whether it's currently valid (see [`IssuerKey::is_valid_at`]).
///
/// - **`Root`** — the sole authority for changing the issuer's own key set
///   (adding or revoking any key, root or operational). Established at
///   registration; day-to-day attestation signing is not its job, though
///   nothing prevents it from also being used that way (see `Operational`
///   below).
/// - **`Operational`** — signs attestations day to day. Can never authorize
///   a key-set change, even its own revocation — see
///   [`IssuerKey::authorizes_key_changes`]. Multiple concurrent operational
///   keys (regions, environments, staged rotation) are ordinary.
///
/// Registration establishes exactly one keypair, which is both the issuer's
/// root key and its first operational key by default (`role: Root` — an
/// `Root` key is always also valid for signing; see
/// [`IssuerKey::may_sign_attestations`]) — zero extra friction for a
/// first-time registrant. A later `issuer.key_added` can register a
/// dedicated `Operational` key at the developer's own pace, letting the
/// root be kept cold from then on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyRole {
    Root,
    Operational,
}

impl KeyRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyRole::Root => "root",
            KeyRole::Operational => "operational",
        }
    }

    pub fn parse(s: &str) -> Option<KeyRole> {
        Some(match s {
            "root" => KeyRole::Root,
            "operational" => KeyRole::Operational,
            _ => return None,
        })
    }
}

/// One key in an issuer's key history (issue #80, decided; implemented here
/// per #84) — the record [`resolve_valid_signing_key`] and
/// [`resolve_valid_root_key`] search. Pure data plus pure point-in-time
/// queries; no I/O, matching #84's own suggested placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuerKey {
    pub key_id: Uuid,
    pub algorithm: String,
    pub public_key: Vec<u8>,
    pub role: KeyRole,
    pub valid_from: OffsetDateTime,
    /// `None` means no expiry — the common case. `Some` is an issuer-chosen
    /// validity window, checked the same way `valid_from` and `revoked_at`
    /// are: only at the point in time being verified against, never "now".
    pub valid_until: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

impl IssuerKey {
    /// "Was this key legitimate for this issuer at `at`" — the central
    /// question #80/#84 exist to answer, deliberately separate from "is
    /// this key legitimate right now." A revoked key stays valid for every
    /// point in time strictly before its revocation; rotating or revoking a
    /// key never retroactively invalidates what it already signed.
    pub fn is_valid_at(&self, at: OffsetDateTime) -> bool {
        if at < self.valid_from {
            return false;
        }
        if let Some(revoked_at) = self.revoked_at {
            if at >= revoked_at {
                return false;
            }
        }
        if let Some(valid_until) = self.valid_until {
            if at >= valid_until {
                return false;
            }
        }
        true
    }

    /// Only a [`KeyRole::Root`] key may author `issuer.key_added`/
    /// `issuer.key_revoked` — an operational key can never authorize a
    /// key-set change, even its own revocation. Callers must still check
    /// [`Self::is_valid_at`] separately: a revoked root key doesn't
    /// authorize anything either.
    pub fn authorizes_key_changes(&self) -> bool {
        self.role == KeyRole::Root
    }

    /// Every key, root or operational, may sign attestations — role only
    /// restricts key-*set* changes (see [`Self::authorizes_key_changes`]),
    /// never attestation-signing authority. Kept as its own named method
    /// rather than inlined at call sites so "can this key sign an
    /// attestation" reads as its own deliberate question, not an
    /// accidental `true` from forgetting to check role at all.
    pub fn may_sign_attestations(&self) -> bool {
        true
    }
}

/// Resolves whether `key_id` was a valid *signing* key (root or
/// operational — see [`IssuerKey::may_sign_attestations`]) for this issuer
/// at `at`, searching `keys` (the issuer's full key history, any order).
/// This is scenario E/F's core question: an attestation's signature is
/// authentic only if its signing key resolves here for the attestation's
/// own `issued_at`, not for "now".
pub fn resolve_valid_signing_key(
    keys: &[IssuerKey],
    key_id: Uuid,
    at: OffsetDateTime,
) -> Option<&IssuerKey> {
    keys.iter()
        .find(|k| k.key_id == key_id && k.is_valid_at(at) && k.may_sign_attestations())
}

/// Resolves whether `key_id` was a valid **root** key for this issuer at
/// `at` — what a `POST .../keys` (add) or `.../keys/{id}/revoke` handler
/// must confirm about whichever key authenticated the request before
/// accepting a key-set change.
pub fn resolve_valid_root_key(
    keys: &[IssuerKey],
    key_id: Uuid,
    at: OffsetDateTime,
) -> Option<&IssuerKey> {
    keys.iter()
        .find(|k| k.key_id == key_id && k.is_valid_at(at) && k.authorizes_key_changes())
}

/// What kind of integrator a registrant is (issue #282, decision #275).
/// Additive only — does not rename `GameId`/`Issuer::Game`/`games`/`game.*`
/// events. Defaults to `Game`, so a caller that omits `category` is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegratorCategory {
    #[default]
    Game,
    App,
    Service,
}

impl IntegratorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntegratorCategory::Game => "game",
            IntegratorCategory::App => "app",
            IntegratorCategory::Service => "service",
        }
    }

    pub fn parse(s: &str) -> Option<IntegratorCategory> {
        Some(match s {
            "game" => IntegratorCategory::Game,
            "app" => IntegratorCategory::App,
            "service" => IntegratorCategory::Service,
            _ => return None,
        })
    }

    /// The claim-vocabulary word this category's issuers use for what #31
    /// originally called "Achievement" (issue #324, decided): `Game`
    /// issuers keep `"achievement"` exactly as-is — zero churn for what
    /// already shipped — `App`/`Service` issuers share `"milestone"`, one
    /// word for both rather than a third one per category, since an app
    /// and a service don't need vocabulary different *from each other*,
    /// only different from gaming's. Used both as the `GlobalId` "kind"
    /// segment (`game:<slug>:achievement:<key>` vs.
    /// `app:<slug>:milestone:<key>`) and as the event-kind prefix
    /// (`achievement.defined` vs. `milestone.defined`) — see
    /// `crates/server/src/achievements.rs`.
    pub fn claim_kind(&self) -> &'static str {
        match self {
            IntegratorCategory::Game => "achievement",
            IntegratorCategory::App | IntegratorCategory::Service => "milestone",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: GameId,
    pub slug: String,
    pub name: String,
    pub developer: String,
    pub registered_at: OffsetDateTime,
    pub status: GameStatus,
    #[serde(default)]
    pub category: IntegratorCategory,
}

/// The first signing key a game registers with (issue #26). Shaped so issue
/// #84 (issuer key lifecycle — rotation, multiple keys, revocation) can
/// extend rather than replace it: this only ever describes the one key
/// recorded at registration time, never a full key history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuerKeyInfo {
    pub key_id: String,
    pub algorithm: String,
    pub public_key: Vec<u8>,
}

/// The capabilities a game declares it wants, presented to the player before
/// they connect their identity — not a grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRegistration {
    pub game: Game,
    pub requested_capabilities: Vec<Capability>,
    pub initial_key: IssuerKeyInfo,
}

/// A credential a game uses to authenticate itself to Avalon (server-to-server),
/// distinct from a player's own session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameCredential {
    pub game_id: GameId,
    pub key_id: String,
}

/// "This identity participates in this game" — nothing more (issue #83). No
/// characters, race, class, level, appearance, or progression: those stay in
/// the game's own database, and this type deliberately has no field for any
/// of them. See `docs/architecture/game-bindings.md`.
///
/// A binding is established by the **player**, through the consent flow
/// (issue #27, `POST /games/{slug}/connect`) — never created by a game
/// unilaterally. Capability grants (`PermissionGrant`, `permissions.rs`) are
/// scoped to a binding: no active binding, no grants, and ending a binding
/// ends every grant under it. Ending a binding does not delete history —
/// `ended_at` records when, it does not remove the row or the durable
/// `game.binding_established`/`game.binding_ended` events behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameBinding {
    pub identity_id: crate::ids::IdentityId,
    pub game_id: GameId,
    pub established_at: OffsetDateTime,
    pub ended_at: Option<OffsetDateTime>,
}

impl GameBinding {
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_status_round_trips_through_its_wire_string() {
        assert_eq!(GameStatus::Active.as_str(), "active");
        assert_eq!(GameStatus::parse("active"), Some(GameStatus::Active));
        assert_eq!(GameStatus::parse("bogus"), None);
    }

    #[test]
    fn integrator_category_round_trips_through_its_wire_string() {
        assert_eq!(IntegratorCategory::Game.as_str(), "game");
        assert_eq!(IntegratorCategory::App.as_str(), "app");
        assert_eq!(IntegratorCategory::Service.as_str(), "service");
        assert_eq!(
            IntegratorCategory::parse("game"),
            Some(IntegratorCategory::Game)
        );
        assert_eq!(
            IntegratorCategory::parse("app"),
            Some(IntegratorCategory::App)
        );
        assert_eq!(
            IntegratorCategory::parse("service"),
            Some(IntegratorCategory::Service)
        );
        assert_eq!(IntegratorCategory::parse("bogus"), None);
    }

    #[test]
    fn integrator_category_defaults_to_game() {
        assert_eq!(IntegratorCategory::default(), IntegratorCategory::Game);
    }

    #[test]
    fn game_status_round_trips_every_variant() {
        for status in [
            GameStatus::Active,
            GameStatus::Suspended,
            GameStatus::Revoked,
            GameStatus::Deprecated,
        ] {
            assert_eq!(GameStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn key_role_round_trips_through_its_wire_string() {
        assert_eq!(KeyRole::Root.as_str(), "root");
        assert_eq!(KeyRole::Operational.as_str(), "operational");
        assert_eq!(KeyRole::parse("root"), Some(KeyRole::Root));
        assert_eq!(KeyRole::parse("operational"), Some(KeyRole::Operational));
        assert_eq!(KeyRole::parse("bogus"), None);
    }

    fn t(hour: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::hours(hour)
    }

    fn key(
        role: KeyRole,
        valid_from: i64,
        valid_until: Option<i64>,
        revoked_at: Option<i64>,
    ) -> IssuerKey {
        IssuerKey {
            key_id: Uuid::new_v4(),
            algorithm: "ed25519".to_string(),
            public_key: vec![0u8; 32],
            role,
            valid_from: t(valid_from),
            valid_until: valid_until.map(t),
            revoked_at: revoked_at.map(t),
        }
    }

    #[test]
    fn a_key_is_invalid_before_its_valid_from() {
        let k = key(KeyRole::Operational, 10, None, None);
        assert!(!k.is_valid_at(t(9)));
        assert!(k.is_valid_at(t(10)));
    }

    #[test]
    fn a_key_is_invalid_at_or_after_its_expiry() {
        let k = key(KeyRole::Operational, 0, Some(10), None);
        assert!(k.is_valid_at(t(9)));
        assert!(!k.is_valid_at(t(10)));
    }

    /// Scenario E (`docs/architecture/games-and-issuers.md`): a claim
    /// signed by k1 in the past stays authentic after k1 is later retired —
    /// rotation never invalidates history.
    #[test]
    fn scenario_e_rotation_never_invalidates_a_historical_claim() {
        let k1 = key(KeyRole::Operational, 0, None, Some(20)); // revoked at hour 20 (rotated)
        let issued_at = t(5);

        assert!(k1.is_valid_at(issued_at), "k1 was valid when it signed");
        assert!(
            resolve_valid_signing_key(std::slice::from_ref(&k1), k1.key_id, issued_at).is_some(),
            "scenario E: a claim signed before rotation stays authentic"
        );
        // And k1 no longer resolves for anything at or after its rotation.
        assert!(resolve_valid_signing_key(std::slice::from_ref(&k1), k1.key_id, t(20)).is_none());
    }

    /// Scenario F (`docs/architecture/games-and-issuers.md`): claims signed
    /// by a compromised key before revocation remain authentic; claims
    /// "signed" after revocation are rejected.
    #[test]
    fn scenario_f_compromise_only_rejects_claims_after_the_revocation_point() {
        let compromised_at = 20;
        let k1 = key(KeyRole::Operational, 0, None, Some(compromised_at));
        let keys = [k1.clone()];

        assert!(
            resolve_valid_signing_key(&keys, k1.key_id, t(compromised_at - 1)).is_some(),
            "scenario F: a claim signed before T remains authentic"
        );
        assert!(
            resolve_valid_signing_key(&keys, k1.key_id, t(compromised_at + 1)).is_none(),
            "scenario F: a claim \"signed\" after T is rejected"
        );
    }

    #[test]
    fn only_a_root_key_authorizes_key_set_changes() {
        let root = key(KeyRole::Root, 0, None, None);
        let operational = key(KeyRole::Operational, 0, None, None);
        assert!(root.authorizes_key_changes());
        assert!(!operational.authorizes_key_changes());

        // But both may sign attestations — role restricts key-set changes
        // only, never attestation-signing authority.
        assert!(root.may_sign_attestations());
        assert!(operational.may_sign_attestations());
    }

    #[test]
    fn a_leaked_operational_key_cannot_authorize_its_own_revocation() {
        // The precise property #80 decided the two-tier split for: an
        // operational key never resolves as a valid *root* key, so it can
        // never author `issuer.key_revoked` against itself or anything
        // else in the set, even while it's still otherwise valid.
        let leaked = key(KeyRole::Operational, 0, None, None);
        assert!(
            resolve_valid_root_key(std::slice::from_ref(&leaked), leaked.key_id, t(5)).is_none()
        );
        // The same key still resolves fine as a signing key, though —
        // being leaked doesn't retroactively un-sign what it already
        // signed, and being operational never gave it root authority in
        // the first place.
        assert!(
            resolve_valid_signing_key(std::slice::from_ref(&leaked), leaked.key_id, t(5)).is_some()
        );
    }

    #[test]
    fn resolve_valid_root_key_finds_the_root_among_a_mixed_key_set() {
        let root = key(KeyRole::Root, 0, None, None);
        let op = key(KeyRole::Operational, 0, None, None);
        let keys = [root.clone(), op.clone()];

        assert_eq!(
            resolve_valid_root_key(&keys, root.key_id, t(5)).map(|k| k.key_id),
            Some(root.key_id)
        );
        assert_eq!(resolve_valid_root_key(&keys, op.key_id, t(5)), None);
    }
}
