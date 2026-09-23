//! Post-compromise rollback: a recovered identity's owner reverses actions an
//! attacker took while holding their access, by appending a signed
//! compensating event that supersedes the original's effect. The original
//! ledger entry is never rewritten or removed.
//!
//! An event is eligible iff it was authored by the identity, its
//! `event_timestamp` lies in `[since, completed_at)` where `completed_at` is
//! the latest completed recovery's completion time and `since` is the
//! caller-declared start of the compromise. Eligibility and reversibility
//! are decided by the pure functions [`extract_target`] and [`assess`]; this
//! module's handlers only gather ledger rows and current state for them.
//!
//! Only reversals that need no other party's action are offered: undoing an
//! addition (friendship, membership) and restoring a voluntary guild leave
//! into a currently open guild.

use avalon_indexer::projections::{friendships as friendship_reads, guild_rosters};
use avalon_protocol::event_payloads::{
    FriendRelationshipReversedPayload, GuildMembershipReversedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::guilds::JoinPolicy;
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::error::AppError;
use crate::guilds::{can_join_directly, MEMBER_ROLE_INDEX};
use crate::handlers::authenticate;
use crate::outbox;
use crate::signature_gate::{canonical_message, require_fresh_signature};
use crate::state::AppState;

/// Signature action tag for [`reverse_event`]. Signed fields, in order:
/// `event_id`, `identity_id`, `since` (the exact string sent).
pub const REVERSE_ACTION_TAG: &str = "rollback.reverse";

const EFFECT_FRIENDSHIP_REMOVED: &str = "friendship_removed";
const EFFECT_MEMBERSHIP_REMOVED: &str = "membership_removed";
const EFFECT_MEMBERSHIP_RESTORED: &str = "membership_restored";

const KIND_FRIEND_ACCEPTED: &str = "friend.accepted";
const KIND_FRIEND_REMOVED: &str = "friend.removed";
const KIND_MEMBER_ADDED: &str = "guild.member_added";
const KIND_MEMBER_REMOVED: &str = "guild.member_removed";

const CANDIDATE_KINDS: [&str; 4] = [
    KIND_FRIEND_ACCEPTED,
    KIND_FRIEND_REMOVED,
    KIND_MEMBER_ADDED,
    KIND_MEMBER_REMOVED,
];

// --- pure eligibility logic ---------------------------------------------

/// Half-open interval `[since, completed_at)` an event's timestamp must fall
/// in to be eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackWindow {
    pub since: OffsetDateTime,
    pub completed_at: OffsetDateTime,
}

impl RollbackWindow {
    /// Rejects an empty or inverted window (`since >= completed_at`).
    pub fn new(since: OffsetDateTime, completed_at: OffsetDateTime) -> Result<Self, AppError> {
        if since >= completed_at {
            return Err(AppError::InvalidRollbackWindow);
        }
        Ok(Self {
            since,
            completed_at,
        })
    }

    pub fn contains(&self, at: OffsetDateTime) -> bool {
        at >= self.since && at < self.completed_at
    }
}

/// One ledger row, reduced to what eligibility needs.
#[derive(Debug, Clone)]
pub struct LedgerEvent {
    pub event_id: Uuid,
    pub kind: String,
    pub issuer: String,
    pub occurred_at: OffsetDateTime,
    pub payload: serde_json::Value,
}

/// What an eligible event touches; determines which current state
/// [`assess`] needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Friend { counterparty: Uuid },
    Guild { guild_id: Uuid },
}

/// The compensating action to apply for a reversible candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReversalPlan {
    RemoveFriendship { counterparty: Uuid },
    RemoveMembership { guild_id: Uuid },
    RestoreMembership { guild_id: Uuid },
}

/// Current guild facts relevant to reversal; `None` in [`Context::guild`]
/// means the guild no longer exists.
#[derive(Debug, Clone, Copy)]
pub struct GuildContext {
    pub owner: Uuid,
    pub join_open: bool,
}

/// Current state a candidate is assessed against.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    pub already_reversed: bool,
    pub friends_now: bool,
    pub is_member_now: bool,
    pub guild: Option<GuildContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub summary: String,
    pub reversible: bool,
    pub reason: Option<String>,
    pub already_reversed: bool,
    pub plan: Option<ReversalPlan>,
}

/// Identity id embedded in an `identity:<id>:self:<verb>` issuer string.
pub fn issuer_identity(issuer: &str) -> Option<Uuid> {
    let mut parts = issuer.split(':');
    if parts.next()? != "identity" {
        return None;
    }
    Uuid::parse_str(parts.next()?).ok()
}

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Option<Uuid> {
    Uuid::parse_str(payload.get(key)?.as_str()?).ok()
}

/// Decides whether `event` is a rollback candidate for `identity` at all
/// (authorship, window, and kind-specific shape) and what it touches.
/// `None` means the event is not listed.
pub fn extract_target(
    identity: Uuid,
    window: &RollbackWindow,
    event: &LedgerEvent,
) -> Option<Target> {
    if issuer_identity(&event.issuer)? != identity || !window.contains(event.occurred_at) {
        return None;
    }
    let payload = &event.payload;
    match event.kind.as_str() {
        KIND_FRIEND_ACCEPTED => {
            let from = payload_uuid(payload, "from")?;
            let to = payload_uuid(payload, "to")?;
            if payload_uuid(payload, "actor")? != identity {
                return None;
            }
            let counterparty = if from == identity { to } else { from };
            (counterparty != identity).then_some(Target::Friend { counterparty })
        }
        KIND_FRIEND_REMOVED => {
            let a = payload_uuid(payload, "a")?;
            let b = payload_uuid(payload, "b")?;
            if payload_uuid(payload, "actor")? != identity {
                return None;
            }
            let counterparty = if a == identity { b } else { a };
            Some(Target::Friend { counterparty })
        }
        KIND_MEMBER_ADDED => {
            if payload_uuid(payload, "identity_id")? != identity {
                return None;
            }
            Some(Target::Guild {
                guild_id: payload_uuid(payload, "guild_id")?,
            })
        }
        KIND_MEMBER_REMOVED => {
            if payload_uuid(payload, "identity_id")? != identity
                || payload_uuid(payload, "actor")? != identity
                || payload.get("reason")?.as_str()? != "left"
            {
                return None;
            }
            Some(Target::Guild {
                guild_id: payload_uuid(payload, "guild_id")?,
            })
        }
        _ => None,
    }
}

fn not_reversible(summary: String, reason: &str) -> Assessment {
    Assessment {
        summary,
        reversible: false,
        reason: Some(reason.to_string()),
        already_reversed: false,
        plan: None,
    }
}

fn reversible(summary: String, plan: ReversalPlan) -> Assessment {
    Assessment {
        summary,
        reversible: true,
        reason: None,
        already_reversed: false,
        plan: Some(plan),
    }
}

/// Decides whether an eligible candidate can be reversed against current
/// state, and how.
pub fn assess(identity: Uuid, event: &LedgerEvent, target: Target, ctx: &Context) -> Assessment {
    let mut assessment = assess_inner(identity, event, target, ctx);
    if ctx.already_reversed {
        assessment.reversible = false;
        assessment.already_reversed = true;
        assessment.reason = Some("This event has already been reversed.".to_string());
        assessment.plan = None;
    }
    assessment
}

fn assess_inner(identity: Uuid, event: &LedgerEvent, target: Target, ctx: &Context) -> Assessment {
    match (event.kind.as_str(), target) {
        (KIND_FRIEND_ACCEPTED, Target::Friend { counterparty }) => {
            let summary = format!("Became friends with {counterparty}");
            if ctx.friends_now {
                reversible(summary, ReversalPlan::RemoveFriendship { counterparty })
            } else {
                not_reversible(summary, "The friendship no longer exists.")
            }
        }
        (KIND_FRIEND_REMOVED, Target::Friend { counterparty }) => not_reversible(
            format!("Removed friend {counterparty}"),
            "A removed friendship needs the other person's agreement to restore; send them a new friend request.",
        ),
        (KIND_MEMBER_ADDED, Target::Guild { guild_id }) => {
            let summary = format!("Joined guild {guild_id}");
            match ctx.guild {
                None => not_reversible(summary, "The guild no longer exists."),
                Some(guild) if guild.owner == identity => not_reversible(
                    summary,
                    "You own this guild; transfer ownership or delete the guild instead.",
                ),
                Some(_) if !ctx.is_member_now => {
                    not_reversible(summary, "You are no longer a member of this guild.")
                }
                Some(_) => reversible(summary, ReversalPlan::RemoveMembership { guild_id }),
            }
        }
        (KIND_MEMBER_REMOVED, Target::Guild { guild_id }) => {
            let summary = format!("Left guild {guild_id}");
            match ctx.guild {
                None => not_reversible(summary, "The guild no longer exists."),
                Some(_) if ctx.is_member_now => {
                    not_reversible(summary, "You are already a member of this guild.")
                }
                Some(guild) if !guild.join_open => not_reversible(
                    summary,
                    "This guild is invite-only; ask a guild officer to invite you again.",
                ),
                Some(_) => reversible(summary, ReversalPlan::RestoreMembership { guild_id }),
            }
        }
        _ => not_reversible(
            format!("{} event", event.kind),
            "This kind of event cannot be reversed.",
        ),
    }
}

// --- DB access ----------------------------------------------------------

struct CompletedRecovery {
    request_id: Uuid,
    completed_at: OffsetDateTime,
}

async fn latest_completed_recovery(
    conn: &mut PgConnection,
    identity: Uuid,
) -> Result<CompletedRecovery, AppError> {
    let row = sqlx::query(
        "SELECT id, completed_at FROM recovery_requests \
         WHERE identity_id = $1 AND status = 'completed' AND completed_at IS NOT NULL \
         ORDER BY completed_at DESC LIMIT 1",
    )
    .bind(identity)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::RollbackNoCompletedRecovery)?;
    Ok(CompletedRecovery {
        request_id: row.try_get("id")?,
        completed_at: row.try_get("completed_at")?,
    })
}

fn parse_since(raw: &str) -> Result<OffsetDateTime, AppError> {
    OffsetDateTime::parse(raw, &Rfc3339).map_err(|_| AppError::InvalidRollbackWindow)
}

fn ledger_event_from_row(row: &sqlx::postgres::PgRow) -> Result<LedgerEvent, sqlx::Error> {
    Ok(LedgerEvent {
        event_id: row.try_get("event_id")?,
        kind: row.try_get("kind")?,
        issuer: row.try_get("issuer")?,
        occurred_at: row.try_get("event_timestamp")?,
        payload: row.try_get("payload")?,
    })
}

/// Whether a compensating event referencing `event_id` exists in the ledger
/// or is still pending in the outbox.
async fn already_reversed(conn: &mut PgConnection, event_id: Uuid) -> Result<bool, AppError> {
    let id = event_id.to_string();
    let hit: bool = sqlx::query_scalar(
        "SELECT EXISTS (\
             SELECT 1 FROM ledger_entries \
             WHERE kind IN ('guild.membership_reversed', 'friend.relationship_reversed') \
               AND payload->>'reverses_event_id' = $1\
         ) OR EXISTS (\
             SELECT 1 FROM protocol_outbox \
             WHERE committed_at IS NULL \
               AND event->>'kind' IN ('guild.membership_reversed', 'friend.relationship_reversed') \
               AND event->'payload'->>'reverses_event_id' = $1\
         )",
    )
    .bind(&id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(hit)
}

async fn load_context(
    conn: &mut PgConnection,
    identity: Uuid,
    event_id: Uuid,
    target: Target,
) -> Result<Context, AppError> {
    let mut ctx = Context {
        already_reversed: already_reversed(conn, event_id).await?,
        ..Context::default()
    };
    match target {
        Target::Friend { counterparty } => {
            ctx.friends_now =
                friendship_reads::are_friends(&mut *conn, identity, counterparty).await?;
        }
        Target::Guild { guild_id } => {
            ctx.is_member_now = guild_rosters::is_member(&mut *conn, guild_id, identity).await?;
            let row = sqlx::query("SELECT owner, join_policy FROM guilds WHERE id = $1")
                .bind(guild_id)
                .fetch_optional(&mut *conn)
                .await?;
            if let Some(row) = row {
                let raw: String = row.try_get("join_policy")?;
                let policy = JoinPolicy::parse(&raw).unwrap_or(JoinPolicy::InviteOnly);
                ctx.guild = Some(GuildContext {
                    owner: row.try_get("owner")?,
                    join_open: can_join_directly(policy),
                });
            }
        }
    }
    Ok(ctx)
}

fn identity_ref(identity_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

fn guild_ref(guild_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("guild", &guild_id.to_string(), "self", verb)
}

// --- HTTP ---------------------------------------------------------------

#[derive(Deserialize, IntoParams)]
pub struct RollbackCandidatesQuery {
    /// RFC 3339 timestamp at which the owner believes the compromise began.
    pub since: String,
}

#[derive(Serialize, ToSchema)]
pub struct RollbackCandidate {
    pub event_id: Uuid,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub occurred_at: OffsetDateTime,
    pub summary: String,
    pub reversible: bool,
    /// Why the event cannot be reversed; `null` when reversible.
    pub reason: Option<String>,
    pub already_reversed: bool,
}

#[derive(Serialize, ToSchema)]
pub struct RollbackCandidatesResponse {
    pub recovery_request_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub recovery_completed_at: OffsetDateTime,
    pub candidates: Vec<RollbackCandidate>,
}

/// `GET /me/rollback/candidates?since=` — events the caller authored in
/// `[since, latest completed recovery)` that a rollback could touch, each
/// marked reversible or not with a reason.
#[utoipa::path(
    get,
    path = "/me/rollback/candidates",
    tag = "devices",
    params(RollbackCandidatesQuery),
    responses((status = 200, body = RollbackCandidatesResponse)),
)]
pub async fn list_rollback_candidates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RollbackCandidatesQuery>,
) -> Result<Json<RollbackCandidatesResponse>, AppError> {
    let identity = authenticate(&state, &headers).await?;
    let since = parse_since(&query.since)?;

    let mut conn = state.pool.acquire().await?;
    let recovery = latest_completed_recovery(&mut conn, identity).await?;
    let window = RollbackWindow::new(since, recovery.completed_at)?;

    let rows = sqlx::query(
        "SELECT event_id, kind, issuer, payload, event_timestamp FROM ledger_entries \
         WHERE issuer LIKE $1 AND kind = ANY($2) \
           AND event_timestamp >= $3 AND event_timestamp < $4 \
         ORDER BY seq",
    )
    .bind(format!("identity:{identity}:self:%"))
    .bind(
        CANDIDATE_KINDS
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>(),
    )
    .bind(window.since)
    .bind(window.completed_at)
    .fetch_all(&mut *conn)
    .await?;

    let mut candidates = Vec::new();
    for row in &rows {
        let event = ledger_event_from_row(row)?;
        let Some(target) = extract_target(identity, &window, &event) else {
            continue;
        };
        let ctx = load_context(&mut conn, identity, event.event_id, target).await?;
        let assessment = assess(identity, &event, target, &ctx);
        candidates.push(RollbackCandidate {
            event_id: event.event_id,
            kind: event.kind,
            occurred_at: event.occurred_at,
            summary: assessment.summary,
            reversible: assessment.reversible,
            reason: assessment.reason,
            already_reversed: assessment.already_reversed,
        });
    }

    Ok(Json(RollbackCandidatesResponse {
        recovery_request_id: recovery.request_id,
        recovery_completed_at: recovery.completed_at,
        candidates,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct ReverseEventRequest {
    /// Same `since` value as the candidates listing (RFC 3339); part of the
    /// signed message.
    pub since: String,
    #[serde(default)]
    pub signing_key_id: Option<Uuid>,
    /// Base64 Ed25519 signature over
    /// `avalon:rollback.reverse:v1:<event_id>:<identity_id>:<since>`.
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ReverseEventResponse {
    pub reversal_event_id: Uuid,
}

/// `POST /me/rollback/{event_id}/reverse` — appends the compensating event
/// for one eligible, reversible candidate atomically with its ledger entry.
/// Requires a fresh signature.
#[utoipa::path(
    post,
    path = "/me/rollback/{event_id}/reverse",
    tag = "devices",
    params(("event_id" = Uuid, Path)),
    request_body = ReverseEventRequest,
    responses((status = 200, body = ReverseEventResponse)),
)]
pub async fn reverse_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    Json(body): Json<ReverseEventRequest>,
) -> Result<Json<ReverseEventResponse>, AppError> {
    let identity = authenticate(&state, &headers).await?;
    let since = parse_since(&body.since)?;

    let message = canonical_message(
        REVERSE_ACTION_TAG,
        &[&event_id.to_string(), &identity.to_string(), &body.since],
    );
    require_fresh_signature(
        &state,
        identity,
        &message,
        body.signing_key_id,
        body.signature.as_deref(),
    )
    .await?;

    let mut tx = state.pool.begin().await?;

    // Serializes concurrent reversals of the same event so the
    // already-reversed check and the append are one atomic step.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("rollback:{event_id}"))
        .execute(&mut *tx)
        .await?;

    let recovery = latest_completed_recovery(&mut tx, identity).await?;
    let window = RollbackWindow::new(since, recovery.completed_at)?;

    let row = sqlx::query(
        "SELECT event_id, kind, issuer, payload, event_timestamp FROM ledger_entries \
         WHERE event_id = $1 AND kind = ANY($2) ORDER BY seq LIMIT 1",
    )
    .bind(event_id)
    .bind(
        CANDIDATE_KINDS
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::RollbackEventNotEligible)?;
    let event = ledger_event_from_row(&row)?;

    let target =
        extract_target(identity, &window, &event).ok_or(AppError::RollbackEventNotEligible)?;
    let ctx = load_context(&mut tx, identity, event.event_id, target).await?;
    let assessment = assess(identity, &event, target, &ctx);
    if assessment.already_reversed {
        return Err(AppError::RollbackAlreadyReversed);
    }
    let plan = match assessment.plan {
        Some(plan) => plan,
        None => {
            return Err(AppError::RollbackNotReversible(
                assessment.reason.unwrap_or_default(),
            ))
        }
    };

    let now = OffsetDateTime::now_utc();
    let reversal_event_id = Uuid::new_v4();
    let mut cleared_main_guild = None;
    let reversal = match plan {
        ReversalPlan::RemoveFriendship { counterparty } => ProtocolEvent {
            id: reversal_event_id,
            kind: ProtocolEventKindVariant::FriendRelationshipReversed
                .as_str()
                .to_string(),
            issuer: identity_ref(identity, "friend_relationship_reversed"),
            subject: identity_ref(counterparty, "friend_relationship_reversed"),
            payload: serde_json::to_value(FriendRelationshipReversedPayload {
                reverses_event_id: event.event_id,
                recovery_request_id: recovery.request_id,
                identity_id: identity,
                counterparty_id: counterparty,
                effect: EFFECT_FRIENDSHIP_REMOVED.to_string(),
            })
            .expect("FriendRelationshipReversedPayload should serialize"),
            timestamp: now,
            version: 1,
        },
        ReversalPlan::RemoveMembership { guild_id }
        | ReversalPlan::RestoreMembership { guild_id } => {
            let restored = matches!(plan, ReversalPlan::RestoreMembership { .. });
            if !restored {
                cleared_main_guild = main_guild_clear_event(&mut tx, identity, guild_id).await?;
            }
            ProtocolEvent {
                id: reversal_event_id,
                kind: ProtocolEventKindVariant::GuildMembershipReversed
                    .as_str()
                    .to_string(),
                issuer: identity_ref(identity, "guild_membership_reversed"),
                subject: guild_ref(guild_id, "guild_membership_reversed"),
                payload: serde_json::to_value(GuildMembershipReversedPayload {
                    reverses_event_id: event.event_id,
                    recovery_request_id: recovery.request_id,
                    guild_id,
                    identity_id: identity,
                    effect: if restored {
                        EFFECT_MEMBERSHIP_RESTORED
                    } else {
                        EFFECT_MEMBERSHIP_REMOVED
                    }
                    .to_string(),
                    role_index: MEMBER_ROLE_INDEX,
                })
                .expect("GuildMembershipReversedPayload should serialize"),
                timestamp: now,
                version: 1,
            }
        }
    };

    if let Some(clear) = &cleared_main_guild {
        state.indexer.apply_in_tx(&mut tx, clear).await?;
        outbox::enqueue(&mut tx, clear).await?;
    }
    state.indexer.apply_in_tx(&mut tx, &reversal).await?;
    outbox::enqueue(&mut tx, &reversal).await?;

    tx.commit().await?;
    if let Some(clear) = &cleared_main_guild {
        state.indexer.apply_after_commit(clear).await?;
    }
    state.indexer.apply_after_commit(&reversal).await?;

    Ok(Json(ReverseEventResponse { reversal_event_id }))
}

/// A `profile.updated` event clearing `main_guild` when it points at the
/// guild the identity is being removed from, so it never dangles.
async fn main_guild_clear_event(
    conn: &mut PgConnection,
    identity: Uuid,
    guild_id: Uuid,
) -> Result<Option<ProtocolEvent>, AppError> {
    let current: Option<Uuid> =
        sqlx::query_scalar("SELECT main_guild FROM profiles WHERE identity_id = $1")
            .bind(identity)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    if current != Some(guild_id) {
        return Ok(None);
    }
    Ok(Some(ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::ProfileUpdated
            .as_str()
            .to_string(),
        issuer: identity_ref(identity, "profile_updated"),
        subject: identity_ref(identity, "profile_updated"),
        payload: crate::handlers::profile_updated_payload(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(None),
        ),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::Duration;

    use super::*;

    fn t(secs: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs)
    }

    fn window() -> RollbackWindow {
        RollbackWindow::new(t(100), t(200)).unwrap()
    }

    fn event(
        identity: Uuid,
        kind: &str,
        at: OffsetDateTime,
        payload: serde_json::Value,
    ) -> LedgerEvent {
        LedgerEvent {
            event_id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: format!("identity:{identity}:self:x"),
            occurred_at: at,
            payload,
        }
    }

    fn friend_accepted(me: Uuid, other: Uuid, at: OffsetDateTime) -> LedgerEvent {
        event(
            me,
            "friend.accepted",
            at,
            json!({ "from": other, "to": me, "actor": me }),
        )
    }

    fn joined(me: Uuid, guild: Uuid, at: OffsetDateTime) -> LedgerEvent {
        event(
            me,
            "guild.member_added",
            at,
            json!({ "guild_id": guild, "identity_id": me, "role_index": 2, "via": "join", "actor": me }),
        )
    }

    fn left(me: Uuid, guild: Uuid, at: OffsetDateTime) -> LedgerEvent {
        event(
            me,
            "guild.member_removed",
            at,
            json!({ "guild_id": guild, "identity_id": me, "reason": "left", "actor": me }),
        )
    }

    #[test]
    fn window_rejects_empty_and_inverted_ranges() {
        assert!(RollbackWindow::new(t(200), t(200)).is_err());
        assert!(RollbackWindow::new(t(300), t(200)).is_err());
        assert!(RollbackWindow::new(t(100), t(200)).is_ok());
    }

    #[test]
    fn window_is_inclusive_of_since_and_exclusive_of_completion() {
        let w = window();
        assert!(!w.contains(t(99)));
        assert!(w.contains(t(100)));
        assert!(w.contains(t(199)));
        assert!(!w.contains(t(200)));
        assert!(!w.contains(t(201)));
    }

    #[test]
    fn issuer_identity_parses_identity_refs_only() {
        let id = Uuid::new_v4();
        assert_eq!(issuer_identity(&format!("identity:{id}:self:v")), Some(id));
        assert_eq!(issuer_identity(&format!("guild:{id}:self:v")), None);
        assert_eq!(issuer_identity("identity:not-a-uuid:self:v"), None);
    }

    #[test]
    fn events_outside_the_window_are_not_candidates() {
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        for at in [t(99), t(200), t(500)] {
            assert_eq!(
                extract_target(me, &window(), &friend_accepted(me, other, at)),
                None
            );
        }
        assert_eq!(
            extract_target(me, &window(), &friend_accepted(me, other, t(100))),
            Some(Target::Friend {
                counterparty: other
            })
        );
    }

    #[test]
    fn events_authored_by_someone_else_are_not_candidates() {
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut ev = friend_accepted(me, other, t(150));
        ev.issuer = format!("identity:{other}:self:x");
        assert_eq!(extract_target(me, &window(), &ev), None);
    }

    #[test]
    fn friend_accepted_reversible_only_while_friends() {
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = friend_accepted(me, other, t(150));
        let target = extract_target(me, &window(), &ev).unwrap();
        let ok = assess(
            me,
            &ev,
            target,
            &Context {
                friends_now: true,
                ..Context::default()
            },
        );
        assert!(ok.reversible);
        assert_eq!(
            ok.plan,
            Some(ReversalPlan::RemoveFriendship {
                counterparty: other
            })
        );
        let gone = assess(me, &ev, target, &Context::default());
        assert!(!gone.reversible && gone.reason.is_some() && gone.plan.is_none());
    }

    #[test]
    fn friend_removed_is_never_restorable() {
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = event(
            me,
            "friend.removed",
            t(150),
            json!({ "a": me, "b": other, "actor": me }),
        );
        let target = extract_target(me, &window(), &ev).unwrap();
        for friends_now in [true, false] {
            let a = assess(
                me,
                &ev,
                target,
                &Context {
                    friends_now,
                    ..Context::default()
                },
            );
            assert!(!a.reversible && a.plan.is_none());
            assert!(a.reason.unwrap().contains("friend request"));
        }
    }

    #[test]
    fn member_added_reversible_when_member_and_not_owner() {
        let (me, guild) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = joined(me, guild, t(150));
        let target = extract_target(me, &window(), &ev).unwrap();
        let ctx = Context {
            is_member_now: true,
            guild: Some(GuildContext {
                owner: Uuid::new_v4(),
                join_open: false,
            }),
            ..Context::default()
        };
        let a = assess(me, &ev, target, &ctx);
        assert_eq!(
            a.plan,
            Some(ReversalPlan::RemoveMembership { guild_id: guild })
        );
    }

    #[test]
    fn member_added_refused_for_guild_owner_missing_guild_or_non_member() {
        let (me, guild) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = joined(me, guild, t(150));
        let target = extract_target(me, &window(), &ev).unwrap();
        let as_owner = Context {
            is_member_now: true,
            guild: Some(GuildContext {
                owner: me,
                join_open: true,
            }),
            ..Context::default()
        };
        assert!(assess(me, &ev, target, &as_owner)
            .reason
            .unwrap()
            .contains("own this guild"));
        let no_guild = Context {
            is_member_now: true,
            ..Context::default()
        };
        assert!(!assess(me, &ev, target, &no_guild).reversible);
        let not_member = Context {
            guild: Some(GuildContext {
                owner: Uuid::new_v4(),
                join_open: true,
            }),
            ..Context::default()
        };
        assert!(!assess(me, &ev, target, &not_member).reversible);
    }

    #[test]
    fn voluntary_leave_restorable_only_into_open_guild_when_not_a_member() {
        let (me, guild) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = left(me, guild, t(150));
        let target = extract_target(me, &window(), &ev).unwrap();
        let guild_ctx = |join_open| {
            Some(GuildContext {
                owner: Uuid::new_v4(),
                join_open,
            })
        };
        let open = Context {
            guild: guild_ctx(true),
            ..Context::default()
        };
        assert_eq!(
            assess(me, &ev, target, &open).plan,
            Some(ReversalPlan::RestoreMembership { guild_id: guild })
        );
        let invite_only = Context {
            guild: guild_ctx(false),
            ..Context::default()
        };
        let a = assess(me, &ev, target, &invite_only);
        assert!(!a.reversible && a.reason.unwrap().contains("invite-only"));
        let already_in = Context {
            guild: guild_ctx(true),
            is_member_now: true,
            ..Context::default()
        };
        assert!(!assess(me, &ev, target, &already_in).reversible);
        assert!(!assess(me, &ev, target, &Context::default()).reversible);
    }

    #[test]
    fn removal_by_another_party_is_not_a_candidate() {
        let (me, guild, officer) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let ev = event(
            me,
            "guild.member_removed",
            t(150),
            json!({ "guild_id": guild, "identity_id": me, "reason": "removed", "actor": officer }),
        );
        assert_eq!(extract_target(me, &window(), &ev), None);
        let ev = event(
            me,
            "guild.member_removed",
            t(150),
            json!({ "guild_id": guild, "identity_id": me, "reason": "left", "actor": officer }),
        );
        assert_eq!(extract_target(me, &window(), &ev), None);
    }

    #[test]
    fn already_reversed_overrides_reversibility_and_drops_the_plan() {
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = friend_accepted(me, other, t(150));
        let target = extract_target(me, &window(), &ev).unwrap();
        let a = assess(
            me,
            &ev,
            target,
            &Context {
                already_reversed: true,
                friends_now: true,
                ..Context::default()
            },
        );
        assert!(a.already_reversed && !a.reversible && a.plan.is_none());
    }

    #[test]
    fn unrelated_kinds_are_not_candidates() {
        let me = Uuid::new_v4();
        let ev = event(me, "profile.updated", t(150), json!({}));
        assert_eq!(extract_target(me, &window(), &ev), None);
    }
}
