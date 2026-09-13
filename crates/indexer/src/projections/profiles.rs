//! The `profiles` projection — display name, discriminator, avatar — built
//! from `identity.created` and `profile.updated`.
//!
//! Unlike the other three projections in this module, this one targets the
//! real `profiles` table `crates/server` already had before this ticket
//! (`crates/server/db/migrations/0001_identity_and_auth`,
//! `.../0005_friend_handles`): closing issue #42 for profiles means
//! retargeting who is allowed to write that table, not standing up a new
//! one. `handlers::register_finish`/`update_profile` no longer INSERT/UPDATE
//! `profiles` themselves — they call
//! [`crate::postgres::PostgresIndexer::apply_in_tx`] with the same
//! `identity.created`/`profile.updated` event they enqueue into the outbox,
//! inside the same transaction, so the identity/profile/outbox rows commit
//! or roll back together. See `docs/architecture/query-and-indexing.md`.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::IndexError;

/// A write against `profiles`, keyed by `identity_id`. `None` for a field
/// means "leave whatever's already stored alone" — `profile.updated` only
/// ever carries the fields a request actually changed (see
/// `handlers::profile_updated_payload`), so a full row isn't always
/// available to decode. `avatar_url`/`bio`/`pronouns`: `Some(None)` is the
/// third state — explicitly cleared, distinct from both "leave alone" and
/// "set to a value." `favorite_genres` has only two states (issue #155): a
/// present key always fully replaces the list (including to empty), since
/// there's no meaningful "clear to null" distinct from "clear to empty" for
/// a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileWrite {
    pub identity_id: Uuid,
    pub display_name: Option<String>,
    pub discriminator: Option<String>,
    pub avatar_url: Option<Option<String>>,
    pub bio: Option<Option<String>>,
    pub favorite_genres: Option<Vec<String>>,
    pub pronouns: Option<Option<String>>,
    /// Expanded self-described fields (issue #372) — same three-state
    /// (`banner_url`/`status`/`timezone`/`theme_color`/`location`) or
    /// two-state (`links`) semantics as their #155 counterparts above.
    pub banner_url: Option<Option<String>>,
    pub status: Option<Option<String>>,
    pub links: Option<Vec<String>>,
    pub timezone: Option<Option<String>>,
    pub theme_color: Option<Option<String>>,
    pub location: Option<Option<String>>,
    /// A self-chosen pointer to one of the identity's own current guild
    /// memberships (no ticket — see
    /// `avalon_protocol::identity::Profile::main_guild`'s doc comment).
    /// Same three-state shape as `bio`/`banner_url`/etc.: `None` untouched,
    /// `Some(None)` explicitly cleared, `Some(Some(id))` set.
    pub main_guild: Option<Option<Uuid>>,
}

/// The identity id embedded in an `identity:<id>:self:<verb>`-shaped
/// `GlobalId` (see `crates/protocol/src/ids.rs`) — `profile.updated`'s
/// payload never repeats the identity id itself (only the changed fields),
/// so it has to come from the event envelope instead.
fn identity_id_from_global_id(id: &avalon_protocol::ids::GlobalId) -> Option<Uuid> {
    let mut parts = id.as_str().splitn(4, ':');
    let namespace = parts.next()?;
    let owner = parts.next()?;
    if namespace != "identity" {
        return None;
    }
    owner.parse().ok()
}

pub fn decode(event: &ProtocolEvent) -> Option<ProfileWrite> {
    match event.kind.as_str() {
        "identity.created" => {
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            let display_name = event.payload.get("display_name")?.as_str()?.to_string();
            let discriminator = event.payload.get("discriminator")?.as_str()?.to_string();
            Some(ProfileWrite {
                identity_id,
                display_name: Some(display_name),
                discriminator: Some(discriminator),
                avatar_url: None,
                bio: None,
                favorite_genres: None,
                pronouns: None,
                banner_url: None,
                status: None,
                links: None,
                timezone: None,
                theme_color: None,
                location: None,
                main_guild: None,
            })
        }
        "profile.updated" => {
            let identity_id = identity_id_from_global_id(&event.subject)?;
            let display_name = event
                .payload
                .get("display_name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let discriminator = event
                .payload
                .get("discriminator")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // `.get` distinguishes "key absent" (untouched) from "key
            // present, value null" (cleared) — `.as_str()` on `null`
            // correctly yields `None` for the clear case too.
            let avatar_url = event
                .payload
                .get("avatar_url")
                .map(|v| v.as_str().map(str::to_string));
            let bio = event
                .payload
                .get("bio")
                .map(|v| v.as_str().map(str::to_string));
            let pronouns = event
                .payload
                .get("pronouns")
                .map(|v| v.as_str().map(str::to_string));
            // `favorite_genres` always fully replaces when present — no
            // per-entry clear state, see `ProfileWrite`'s doc comment.
            let favorite_genres = event.payload.get("favorite_genres").map(|v| {
                v.as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|g| g.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            });
            let banner_url = event
                .payload
                .get("banner_url")
                .map(|v| v.as_str().map(str::to_string));
            let status = event
                .payload
                .get("status")
                .map(|v| v.as_str().map(str::to_string));
            // `links` always fully replaces when present, same as
            // `favorite_genres` — no per-entry clear state.
            let links = event.payload.get("links").map(|v| {
                v.as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|l| l.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            });
            let timezone = event
                .payload
                .get("timezone")
                .map(|v| v.as_str().map(str::to_string));
            let theme_color = event
                .payload
                .get("theme_color")
                .map(|v| v.as_str().map(str::to_string));
            let location = event
                .payload
                .get("location")
                .map(|v| v.as_str().map(str::to_string));
            // Same "`.get` distinguishes absent from explicit null"
            // reasoning as `avatar_url`/`bio` above, plus a UUID parse — the
            // writer (`handlers::profile_updated_payload`) only ever emits a
            // well-formed guild id string or `null`, so an unparseable
            // value here would mean data corruption, not a client error;
            // treated as "clear" rather than failing decode.
            let main_guild = event
                .payload
                .get("main_guild")
                .map(|v| v.as_str().and_then(|s| s.parse().ok()));
            Some(ProfileWrite {
                identity_id,
                display_name,
                discriminator,
                avatar_url,
                bio,
                favorite_genres,
                pronouns,
                banner_url,
                status,
                links,
                timezone,
                theme_color,
                location,
                main_guild,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &ProfileWrite,
) -> Result<(), IndexError> {
    let avatar_url_provided = write.avatar_url.is_some();
    let avatar_url = write.avatar_url.clone().flatten();
    let bio_provided = write.bio.is_some();
    let bio = write.bio.clone().flatten();
    let pronouns_provided = write.pronouns.is_some();
    let pronouns = write.pronouns.clone().flatten();
    let genres_provided = write.favorite_genres.is_some();
    let favorite_genres = write.favorite_genres.clone().unwrap_or_default();
    let banner_url_provided = write.banner_url.is_some();
    let banner_url = write.banner_url.clone().flatten();
    let status_provided = write.status.is_some();
    let status = write.status.clone().flatten();
    let links_provided = write.links.is_some();
    let links = write.links.clone().unwrap_or_default();
    let timezone_provided = write.timezone.is_some();
    let timezone = write.timezone.clone().flatten();
    let theme_color_provided = write.theme_color.is_some();
    let theme_color = write.theme_color.clone().flatten();
    let location_provided = write.location.is_some();
    let location = write.location.clone().flatten();
    let main_guild_provided = write.main_guild.is_some();
    let main_guild = write.main_guild.flatten();

    // `display_name`/`discriminator` are NOT NULL columns, but this write
    // can carry neither (a `profile.updated` that only touched e.g. `bio`)
    // — Postgres validates NOT NULL on the tentative INSERT row even when
    // ON CONFLICT will redirect to the UPDATE branch below, so the VALUES
    // clause can't pass NULL through directly the way avatar_url/bio/
    // favorite_genres/pronouns do. COALESCE to '' there (never actually
    // read back: the UPDATE branch's CASE ignores it and keeps
    // profiles.display_name/discriminator whenever $2/$3 is NULL).
    sqlx::query(
        r#"
        INSERT INTO profiles (
            identity_id, display_name, discriminator, avatar_url,
            bio, favorite_genres, pronouns, banner_url, status, links,
            timezone, theme_color, location, main_guild
        )
        VALUES (
            $1, COALESCE($2, ''), COALESCE($3, ''), CASE WHEN $5 THEN $4 ELSE NULL END,
            CASE WHEN $7 THEN $6 ELSE NULL END,
            CASE WHEN $9 THEN $8 ELSE '{}' END,
            CASE WHEN $11 THEN $10 ELSE NULL END,
            CASE WHEN $13 THEN $12 ELSE NULL END,
            CASE WHEN $15 THEN $14 ELSE NULL END,
            CASE WHEN $17 THEN $16 ELSE '{}' END,
            CASE WHEN $19 THEN $18 ELSE NULL END,
            CASE WHEN $21 THEN $20 ELSE NULL END,
            CASE WHEN $23 THEN $22 ELSE NULL END,
            CASE WHEN $25 THEN $24 ELSE NULL END
        )
        ON CONFLICT (identity_id) DO UPDATE SET
            display_name = CASE WHEN $2 IS NOT NULL THEN EXCLUDED.display_name ELSE profiles.display_name END,
            discriminator = CASE WHEN $3 IS NOT NULL THEN EXCLUDED.discriminator ELSE profiles.discriminator END,
            avatar_url = CASE WHEN $5 THEN EXCLUDED.avatar_url ELSE profiles.avatar_url END,
            bio = CASE WHEN $7 THEN EXCLUDED.bio ELSE profiles.bio END,
            favorite_genres = CASE WHEN $9 THEN EXCLUDED.favorite_genres ELSE profiles.favorite_genres END,
            pronouns = CASE WHEN $11 THEN EXCLUDED.pronouns ELSE profiles.pronouns END,
            banner_url = CASE WHEN $13 THEN EXCLUDED.banner_url ELSE profiles.banner_url END,
            status = CASE WHEN $15 THEN EXCLUDED.status ELSE profiles.status END,
            links = CASE WHEN $17 THEN EXCLUDED.links ELSE profiles.links END,
            timezone = CASE WHEN $19 THEN EXCLUDED.timezone ELSE profiles.timezone END,
            theme_color = CASE WHEN $21 THEN EXCLUDED.theme_color ELSE profiles.theme_color END,
            location = CASE WHEN $23 THEN EXCLUDED.location ELSE profiles.location END,
            main_guild = CASE WHEN $25 THEN EXCLUDED.main_guild ELSE profiles.main_guild END
        "#,
    )
    .bind(write.identity_id)
    .bind(&write.display_name)
    .bind(&write.discriminator)
    .bind(&avatar_url)
    .bind(avatar_url_provided)
    .bind(&bio)
    .bind(bio_provided)
    .bind(&favorite_genres)
    .bind(genres_provided)
    .bind(&pronouns)
    .bind(pronouns_provided)
    .bind(&banner_url)
    .bind(banner_url_provided)
    .bind(&status)
    .bind(status_provided)
    .bind(&links)
    .bind(links_provided)
    .bind(&timezone)
    .bind(timezone_provided)
    .bind(&theme_color)
    .bind(theme_color_provided)
    .bind(&location)
    .bind(location_provided)
    .bind(main_guild)
    .bind(main_guild_provided)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;
    use time::OffsetDateTime;

    use super::*;

    fn identity_created_event(identity_id: Uuid) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
            subject: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
            payload: serde_json::json!({
                "identity_id": identity_id,
                "display_name": "nova",
                "discriminator": "4821",
            }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    fn profile_updated_event(identity_id: Uuid, payload: serde_json::Value) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "profile.updated".to_string(),
            issuer: GlobalId::new(
                "identity",
                &identity_id.to_string(),
                "self",
                "profile_updated",
            ),
            subject: GlobalId::new(
                "identity",
                &identity_id.to_string(),
                "self",
                "profile_updated",
            ),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_identity_created_into_a_full_write() {
        let identity_id = Uuid::new_v4();
        let write = decode(&identity_created_event(identity_id)).unwrap();
        assert_eq!(write.identity_id, identity_id);
        assert_eq!(write.display_name.as_deref(), Some("nova"));
        assert_eq!(write.discriminator.as_deref(), Some("4821"));
        assert_eq!(write.avatar_url, None);
    }

    #[test]
    fn decodes_profile_updated_display_name_only() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(
            identity_id,
            serde_json::json!({ "display_name": "vega", "discriminator": "1122" }),
        );
        let write = decode(&event).unwrap();
        assert_eq!(write.identity_id, identity_id);
        assert_eq!(write.display_name.as_deref(), Some("vega"));
        assert_eq!(write.discriminator.as_deref(), Some("1122"));
        assert_eq!(write.avatar_url, None);
    }

    #[test]
    fn decodes_a_cleared_avatar_as_some_none() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "avatar_url": null }));
        let write = decode(&event).unwrap();
        assert_eq!(write.avatar_url, Some(None));
        assert_eq!(write.display_name, None);
    }

    #[test]
    fn decodes_a_set_avatar() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(
            identity_id,
            serde_json::json!({ "avatar_url": "https://example.com/a.png" }),
        );
        let write = decode(&event).unwrap();
        assert_eq!(
            write.avatar_url,
            Some(Some("https://example.com/a.png".to_string()))
        );
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        let event = profile_updated_event(Uuid::new_v4(), serde_json::json!({}));
        let mut other = event.clone();
        other.kind = "guild.created".to_string();
        assert!(decode(&other).is_none());
    }

    #[test]
    fn identity_id_from_global_id_rejects_a_non_identity_namespace() {
        let global_id = GlobalId::new("guild", &Uuid::new_v4().to_string(), "self", "created");
        assert_eq!(identity_id_from_global_id(&global_id), None);
    }

    #[test]
    fn decodes_a_cleared_bio() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "bio": null }));
        let write = decode(&event).unwrap();
        assert_eq!(write.bio, Some(None));
    }

    #[test]
    fn decodes_a_set_bio() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "bio": "hello there" }));
        let write = decode(&event).unwrap();
        assert_eq!(write.bio, Some(Some("hello there".to_string())));
    }

    #[test]
    fn decodes_favorite_genres_as_a_full_replace() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(
            identity_id,
            serde_json::json!({ "favorite_genres": ["rpg", "puzzle"] }),
        );
        let write = decode(&event).unwrap();
        assert_eq!(
            write.favorite_genres,
            Some(vec!["rpg".to_string(), "puzzle".to_string()])
        );
    }

    #[test]
    fn decodes_favorite_genres_cleared_to_empty() {
        let identity_id = Uuid::new_v4();
        let event =
            profile_updated_event(identity_id, serde_json::json!({ "favorite_genres": [] }));
        let write = decode(&event).unwrap();
        assert_eq!(write.favorite_genres, Some(Vec::new()));
    }

    #[test]
    fn untouched_favorite_genres_decodes_to_none() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "bio": "hi" }));
        let write = decode(&event).unwrap();
        assert_eq!(write.favorite_genres, None);
    }

    #[test]
    fn decodes_a_cleared_main_guild() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "main_guild": null }));
        let write = decode(&event).unwrap();
        assert_eq!(write.main_guild, Some(None));
    }

    #[test]
    fn decodes_a_set_main_guild() {
        let identity_id = Uuid::new_v4();
        let guild_id = Uuid::new_v4();
        let event = profile_updated_event(
            identity_id,
            serde_json::json!({ "main_guild": guild_id.to_string() }),
        );
        let write = decode(&event).unwrap();
        assert_eq!(write.main_guild, Some(Some(guild_id)));
    }

    #[test]
    fn untouched_main_guild_decodes_to_none() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "bio": "hi" }));
        let write = decode(&event).unwrap();
        assert_eq!(write.main_guild, None);
    }

    #[test]
    fn decodes_a_cleared_pronouns() {
        let identity_id = Uuid::new_v4();
        let event = profile_updated_event(identity_id, serde_json::json!({ "pronouns": null }));
        let write = decode(&event).unwrap();
        assert_eq!(write.pronouns, Some(None));
    }

    #[test]
    fn identity_id_from_global_id_parses_the_owner_segment() {
        let identity_id = Uuid::new_v4();
        let global_id = GlobalId::new(
            "identity",
            &identity_id.to_string(),
            "self",
            "profile_updated",
        );
        assert_eq!(identity_id_from_global_id(&global_id), Some(identity_id));
    }
}
