//! The shared read-side authorization helper issue #87 asks for — the
//! counterpart to `crate::authz::require_capability` (issue #28) for reads:
//! every read path that exposes one identity's data to a *different*
//! identity (as opposed to an integrator, which goes through its own
//! capability-grant check) should call [`is_visible`] rather than deciding
//! for itself who gets to see what.
//!
//! **What this composes, and what it doesn't.** [`is_visible`] answers the
//! viewer-*relationship* question — stranger, any authenticated identity,
//! friend, fellow guild member, or nobody-but-the-subject — driven by a
//! stored [`Visibility`] setting. It says nothing about integrator access;
//! an integrator's capability grant is a wholly separate, already-existing
//! mechanism (`crate::authz::require_capability`) and this module never
//! composes with it — the two dimensions (viewer relationship, and "which
//! integrator was granted what") are independent, matching this issue's
//! own "a game's capability grant never widens what non-game viewers can
//! see" invariant: neither should ever fold into the other.
//!
//! **Scope of this first pass.** Two resources are wired through this
//! helper so far: presence (`crate::presence`, previously a hardcoded
//! friends-only rule — now that same default, but player-configurable via
//! `profiles.presence_visibility`) and guild rosters (`crate::guilds::list_members`,
//! previously entirely unscoped — now gated by `guilds.roster_visibility`,
//! a guild-level setting). Profile fields already have their own
//! established field-level exposure system (issue #403); there is no
//! endpoint today that lets one identity read another's *friends list* at
//! all, so there's nothing to gate there yet; achievement history's own
//! per-claim hide/feature control is a separate, larger feature this pass
//! doesn't attempt. Extending `is_visible` to more resources as their own
//! read paths need it is the intended shape going forward, not a
//! redesign.

use avalon_indexer::projections::friendships as friendship_reads;
use avalon_indexer::projections::guild_rosters;
use avalon_protocol::permissions::Visibility;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// Parses a stored visibility string, defaulting to [`Visibility::Public`]
/// on anything unrecognized — the same "never guess, but never hard-fail a
/// read over a stored value we don't understand" posture
/// `crates/server/src/guilds.rs`'s `JoinPolicy::parse` already takes for
/// the identical class of problem (an enum stored as free text, no
/// database CHECK). A row this server itself wrote should never actually
/// hit this fallback; it exists for forward/backward compatibility with a
/// value a newer or older binary wrote.
pub(crate) fn parse_visibility(raw: &str) -> Visibility {
    raw.parse().unwrap_or(Visibility::Public)
}

/// Is `subject`'s (or, for a guild-scoped resource with no single subject
/// identity, the resource's own) data — gated by `visibility` — visible to
/// `viewer`?
///
/// - `viewer`: `None` for an unauthenticated caller.
/// - `subject`: the identity this resource is about, if it's about one
///   identity (`None` for a guild-level resource like a roster, which
///   isn't "about" any single member).
/// - `guild_id`: the guild this resource belongs to, if [`Visibility::GuildMembers`]
///   is meaningful for it — required for that variant to ever return
///   `true` for anyone but the subject.
///
/// The subject always sees their own data regardless of `visibility` —
/// checked before looking at the setting at all, so a subject who set
/// their own presence to `Private` never gets exposed to a bug in this
/// function's own logic locking them out of it.
pub(crate) async fn is_visible(
    state: &AppState,
    visibility: Visibility,
    viewer: Option<Uuid>,
    subject: Option<Uuid>,
    guild_id: Option<Uuid>,
) -> Result<bool, AppError> {
    if let (Some(viewer_id), Some(subject_id)) = (viewer, subject) {
        if viewer_id == subject_id {
            return Ok(true);
        }
    }

    match visibility {
        Visibility::Public => Ok(true),
        Visibility::AuthenticatedOnly => Ok(viewer.is_some()),
        Visibility::Friends => match (viewer, subject) {
            (Some(viewer_id), Some(subject_id)) => are_friends(state, viewer_id, subject_id).await,
            _ => Ok(false),
        },
        Visibility::GuildMembers => match (viewer, guild_id) {
            (Some(viewer_id), Some(guild_id)) => is_guild_member(state, guild_id, viewer_id).await,
            _ => Ok(false),
        },
        Visibility::Private => Ok(false),
    }
}

async fn are_friends(state: &AppState, a: Uuid, b: Uuid) -> Result<bool, AppError> {
    Ok(friendship_reads::are_friends(&state.pool, a, b).await?)
}

async fn is_guild_member(
    state: &AppState,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<bool, AppError> {
    Ok(guild_rosters::is_member(&state.pool, guild_id, identity_id).await?)
}

/// Sets `identity_id`'s own `profiles.presence_visibility` (issue #87) —
/// called from `handlers::update_profile`, outside that request's own
/// transaction and with no `profile.updated` payload entry, the same
/// not-durable-history treatment `discovery::set_discoverable` already
/// gives `discoverable`. The caller is responsible for validating `raw`
/// against [`Visibility`]'s own vocabulary first (`AppError::InvalidVisibility`
/// on a bad value) — this function only ever writes what it's given.
pub(crate) async fn set_presence_visibility(
    state: &AppState,
    identity_id: Uuid,
    raw: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE profiles SET presence_visibility = $2 WHERE identity_id = $1")
        .bind(identity_id)
        .bind(raw)
        .execute(&state.pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! `parse_visibility` is the one piece of this module worth testing
    //! without a live database — everything else in `is_visible` needs
    //! real friendship/membership rows, covered by
    //! `crates/server/tests/visibility.rs`'s live matrix test instead.

    use super::*;

    #[test]
    fn parse_visibility_round_trips_every_known_variant() {
        for (raw, expected) in [
            ("public", Visibility::Public),
            ("authenticated_only", Visibility::AuthenticatedOnly),
            ("friends", Visibility::Friends),
            ("guild_members", Visibility::GuildMembers),
            ("private", Visibility::Private),
        ] {
            assert_eq!(parse_visibility(raw), expected);
        }
    }

    #[test]
    fn parse_visibility_falls_back_to_public_on_garbage() {
        assert_eq!(parse_visibility("not-a-real-value"), Visibility::Public);
    }
}
