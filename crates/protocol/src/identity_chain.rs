//! Per-identity event chains and the deterministic conflict rule layer-1
//! data (profile, friends, guild membership) is resolved by.
//!
//! A ledger already orders every event globally, by arrival at whichever
//! node committed it first — but two nodes that each accept a different
//! signed edit to the same identity field before either has seen the
//! other's can commit them in different global orders. Layer-1 state must
//! still converge, so it cannot be defined as "whatever the ledger says
//! came first." Instead every layer-1 event belongs to its own identity's
//! chain — a per-identity sequence number plus a hash pointer to the
//! previous event in that same chain — and any two nodes that see the same
//! set of events, however they arrived, apply the same rule and reach the
//! same current state.
//!
//! This module is pure logic: no I/O, no Postgres, no signature
//! verification (that already happens before an event is accepted at all
//! — see `avalon-docs/protocol/protocol-events.md`).
//! It answers exactly one question: given a set of events that all claim
//! the same position in an identity's chain, which one wins, and does the
//! identity need to be frozen instead.

use std::collections::BTreeMap;

use time::{Duration, OffsetDateTime};

use uuid::Uuid;

use crate::events::{ProtocolEventKind, ProtocolEventKindVariant};
use crate::identity_id::IdentityId;
use crate::signing_bytes::{tags, Builder, Envelope, HashAlgo, SigningBytesError};

/// A sha256 digest identifying one chained event's content — the hash a
/// later event's `prev_hash` points back to. Hex-encoded on the wire (see
/// [`IdentityChainPosition`] in `crate::events`); kept as raw bytes here
/// since every comparison and tie-break in this module works on the bytes.
pub type EventHash = [u8; 32];

/// Every field the chain hash covers; `payload_hash` is the SHA-256 of the canonical payload,
/// so the position never rides inside the hashed payload.
pub struct ChainHashInput<'a> {
    pub identity_id: &'a IdentityId,
    pub seq: u64,
    pub prev_hash: Option<&'a EventHash>,
    pub event_id: Uuid,
    pub kind: &'a str,
    pub issuer: &'a str,
    pub subject: &'a str,
    pub event_version: u32,
    pub timestamp_micros: i64,
    pub payload_hash: &'a [u8; 32],
    /// Layout version, rules version, hash algorithm and extensions recorded with the event.
    pub envelope: &'a Envelope,
}

/// The exact bytes the chain hash is the digest of. Layout: tag `avalon.identity.chain_event`,
/// header and extensions as in [`crate::signing_bytes`], identity id (32 raw), `seq` u64, event id
/// (16 raw), kind, issuer and subject `str`, the event's own version u32, event time as `i64` unix
/// microseconds, `hash_algo` u8, `prev_hash` (u8 flag 0, or 1 then 32 raw), payload hash (32 raw).
pub fn chain_event_signing_bytes(input: &ChainHashInput<'_>) -> Result<Vec<u8>, SigningBytesError> {
    let builder = Builder::with_envelope(tags::IDENTITY_CHAIN_EVENT, input.envelope)
        .fixed(input.identity_id.as_bytes())
        .u64(input.seq)
        .uuid(input.event_id)
        .str(input.kind)
        .str(input.issuer)
        .str(input.subject)
        .u32(input.event_version)
        .i64(input.timestamp_micros)
        .hash_algo(input.envelope.hash_algo);
    let builder = match input.prev_hash {
        Some(hash) => builder.u8(1).hash(hash),
        None => builder.u8(0),
    };
    builder.hash(input.payload_hash).finish()
}

/// The chain hash: the envelope's hash algorithm over [`chain_event_signing_bytes`]. Every honest
/// node computes the same value for the same event without coordinating on anything but its bytes.
pub fn compute_event_hash(input: &ChainHashInput<'_>) -> Result<EventHash, SigningBytesError> {
    Ok(input
        .envelope
        .hash_algo
        .digest(&chain_event_signing_bytes(input)?))
}

/// What an owner-signed key event's signature establishes. The chain hash of such an event is a
/// function of these fields and its position alone, so one signed event has exactly one hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEventContent {
    /// `identity.signing_key_added` by device grant.
    DeviceGrant {
        signing_key_id: Uuid,
        public_key: [u8; 32],
        approved_by_signing_key_id: Uuid,
        grant_id: Uuid,
        approval_signature: [u8; 64],
    },
    /// `identity.signing_key_revoked`.
    Revocation {
        signing_key_id: Uuid,
        revoked_by_signing_key_id: Uuid,
        signature: [u8; 64],
    },
}

/// Every field the chain hash of a key event covers.
pub struct KeyEventHashInput<'a> {
    pub identity_id: &'a IdentityId,
    pub seq: u64,
    pub prev_hash: Option<&'a EventHash>,
    pub content: &'a KeyEventContent,
}

/// The exact bytes a key event's chain hash is the digest of. Layout: tag `avalon.identity.key_event`,
/// header and extensions as in [`crate::signing_bytes`] (this node's rules, no extensions), identity id
/// (32 raw), `seq` u64, `hash_algo` u8, `prev_hash` (u8 flag 0, or 1 then 32 raw), a content `u8`
/// (1 device grant, 2 revocation), then for a grant the key id and approver key id (uuid), the key,
/// the grant id (uuid) and the approval signature, and for a revocation the key id and revoker key id
/// (uuid) and the signature. The event id, time, issuer, subject, label and the position's own
/// envelope are not covered: no signature covers them.
pub fn key_event_signing_bytes(
    input: &KeyEventHashInput<'_>,
) -> Result<Vec<u8>, SigningBytesError> {
    let builder = Builder::new(tags::IDENTITY_KEY_EVENT, 1)
        .fixed(input.identity_id.as_bytes())
        .u64(input.seq)
        .hash_algo(HashAlgo::Sha256);
    let builder = match input.prev_hash {
        Some(hash) => builder.u8(1).hash(hash),
        None => builder.u8(0),
    };
    match input.content {
        KeyEventContent::DeviceGrant {
            signing_key_id,
            public_key,
            approved_by_signing_key_id,
            grant_id,
            approval_signature,
        } => builder
            .u8(1)
            .uuid(*signing_key_id)
            .uuid(*approved_by_signing_key_id)
            .key(public_key)
            .uuid(*grant_id)
            .signature(approval_signature),
        KeyEventContent::Revocation {
            signing_key_id,
            revoked_by_signing_key_id,
            signature,
        } => builder
            .u8(2)
            .uuid(*signing_key_id)
            .uuid(*revoked_by_signing_key_id)
            .signature(signature),
    }
    .finish()
}

/// The chain hash of a key event: SHA-256 over [`key_event_signing_bytes`].
pub fn compute_key_event_hash(
    input: &KeyEventHashInput<'_>,
) -> Result<EventHash, SigningBytesError> {
    Ok(HashAlgo::Sha256.digest(&key_event_signing_bytes(input)?))
}

/// How a layer-1 event kind participates in its identity's chain. Drives
/// every rule in this module — see each variant's own doc comment for why
/// it needs different treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    /// An ordinary self-descriptive edit (a profile field, a friend action,
    /// joining/leaving a guild). Two concurrent edits are a normal, expected
    /// occurrence — e.g. the same person editing their bio from two
    /// devices while offline — and are resolved by timestamp, not treated
    /// as an attack.
    OrdinaryEdit,
    /// An action that, once applied, can never be reverted by an older or
    /// merely-concurrent event — a revocation, or a rollback compensating
    /// event. Undoing one because some other edit happened to claim an
    /// earlier or equal timestamp would let a stale or racing event
    /// resurrect something the identity's owner deliberately tore down.
    Monotonic,
    /// A key rotation, recovery-flow transition, or signing-key revocation.
    /// These change *which key can author this identity's future events at
    /// all* — picking a winner by timestamp the way an ordinary edit is
    /// resolved would mean an attacker who forges a favorable timestamp
    /// could win a race to control the identity. Two genuinely concurrent
    /// events in this class are never silently resolved; they mark the
    /// identity forked instead (see [`ChainOutcome::Forked`]).
    ChainCritical,
}

impl ActionClass {
    /// Classifies a known event kind, or `None` for a kind that isn't part
    /// of any identity's chain at all (achievement issuance, integrator/
    /// issuer registration, and anything else whose ordering is already
    /// fully determined by its own issuer's key rather than an identity's
    /// own chain).
    pub fn classify(kind: &ProtocolEventKind) -> Option<ActionClass> {
        let ProtocolEventKind::Known(variant) = kind else {
            return None;
        };
        use ProtocolEventKindVariant as K;
        match variant {
            K::ProfileUpdated
            | K::FriendRequested
            | K::FriendAccepted
            | K::FriendRemoved
            | K::GuildMemberAdded
            | K::GuildMemberRemoved
            | K::IdentityPasskeyRegistered
            | K::IdentityPasskeyRevoked => Some(ActionClass::OrdinaryEdit),

            K::FriendRelationshipReversed | K::GuildMembershipReversed => {
                Some(ActionClass::Monotonic)
            }

            K::IdentitySigningKeyAdded
            | K::IdentitySigningKeyRevoked
            | K::IdentityRecoveryConfigured
            | K::IdentityRecovered => Some(ActionClass::ChainCritical),

            // Requests, guardian approvals and cancellations are public markers: a guardian never
            // signs on the owner's chain, and an unauthenticated request must not be able to fork it.
            K::IdentityRecoveryRequested
            | K::IdentityRecoveryApproved
            | K::IdentityRecoveryCancelled => None,

            // identity.created is always seq 1 of a brand-new chain — there
            // is no predecessor it could conflict over.
            K::IdentityCreated => None,

            _ => None,
        }
    }
}

/// Who is authoritative over the fields that made this event, for the
/// purpose of this rule. Distinct from "who signed the wire bytes" — see
/// this module's own report for what the codebase actually does today
/// (most layer-1 events are network-attributed, not identity-signed, per
/// `docs/projects/backend-server/architecture/protocol-events.md`); this
/// enum documents which case a chained event is, not how it got verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAuthority {
    /// Signed by the identity's own Ed25519 event-signing key. Only
    /// `identity.created` and the signing-key add/revoke events currently
    /// do this.
    OwnerSigned,
    /// Attributed to the identity by the node that accepted it, after the
    /// identity's *session* (WebAuthn-authenticated) requested the action.
    /// The node never invents the request, but the identity's own key never
    /// touches these bytes either — session hijack is the relevant threat
    /// model, not a forged signature. Friend and guild-membership events
    /// are this case today.
    AuthenticatedSession,
}

/// One event's position in its identity's chain, plus enough of the event
/// itself to apply the conflict rule. Built by a caller (the server, or a
/// test) from a `ProtocolEvent` plus its `ActionClass`/`EventAuthority`
/// classification — this module never inspects a raw `ProtocolEvent`
/// directly, so it stays decoupled from the wire type and from signature
/// verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainedEvent {
    pub seq: u64,
    pub prev_hash: Option<EventHash>,
    pub event_hash: EventHash,
    /// The signer's own claimed timestamp, already clamped by
    /// [`clamp_timestamp`] — never the raw untrusted value. Only ever used
    /// as a tiebreaker between genuinely concurrent branches, never as the
    /// primary ordering signal (`seq` + `prev_hash` are).
    pub timestamp: OffsetDateTime,
    pub class: ActionClass,
    pub authority: EventAuthority,
}

/// Bounds a receiving node clamps a signer-claimed timestamp against
/// before using it anywhere in the conflict rule. A timestamp is
/// signer-set data, not a trusted clock reading — an event author (or
/// whoever relayed it) could claim any value at all, and the tiebreaker
/// role timestamps play here means a forged far-future timestamp would
/// otherwise let one branch always win.
#[derive(Debug, Clone, Copy)]
pub struct ClockSkewBounds {
    /// How far ahead of the receiving node's own clock a claimed timestamp
    /// may be before it's rejected outright, rather than merely
    /// disadvantaged in a tiebreak.
    pub max_future_skew: Duration,
    /// How far behind the receiving node's own clock a claimed timestamp
    /// may be. `None` means no lower bound is enforced — a genuinely old
    /// event (replayed from deep history, or synced from a node that was
    /// offline for a while) is not itself suspicious the way a future
    /// timestamp is.
    pub max_past_skew: Option<Duration>,
}

impl Default for ClockSkewBounds {
    /// Five minutes of future skew (generous enough for real clock drift
    /// between independently-operated nodes, tight enough that it can't be
    /// used to consistently win a tiebreak against an honest, roughly
    /// synchronized event), no past bound.
    fn default() -> Self {
        ClockSkewBounds {
            max_future_skew: Duration::minutes(5),
            max_past_skew: None,
        }
    }
}

/// Why a claimed timestamp was rejected by [`clamp_timestamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampError {
    /// The claimed timestamp is further ahead of the receiving node's
    /// clock than `max_future_skew` allows — the forged-far-future-value
    /// case this whole check exists for.
    TooFarInFuture,
    /// The claimed timestamp is further behind the receiving node's clock
    /// than `max_past_skew` allows (only possible when a bound is set).
    TooFarInPast,
}

/// Validates `claimed` against `node_now` and `bounds`, returning it
/// unchanged on success. Deliberately not a silent clamp-to-boundary: a
/// timestamp this far off isn't "probably fine, just cap it," it's grounds
/// to reject the event outright and let the normal event-acceptance path
/// surface that to the caller, the same posture an invalid signature gets.
pub fn clamp_timestamp(
    claimed: OffsetDateTime,
    node_now: OffsetDateTime,
    bounds: &ClockSkewBounds,
) -> Result<OffsetDateTime, TimestampError> {
    if claimed > node_now && claimed - node_now > bounds.max_future_skew {
        return Err(TimestampError::TooFarInFuture);
    }
    if let Some(max_past) = bounds.max_past_skew {
        if node_now > claimed && node_now - claimed > max_past {
            return Err(TimestampError::TooFarInPast);
        }
    }
    Ok(claimed)
}

/// The result of resolving one position (`seq`) in an identity's chain
/// where more than one candidate event claimed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution<'a> {
    /// Exactly one candidate wins and becomes the chain's new head at this
    /// `seq`; every other candidate at the same `seq` is superseded (kept
    /// in history — nothing is deleted — but not reflected in current
    /// state).
    Resolved(&'a ChainedEvent),
    /// Two or more [`ActionClass::ChainCritical`] events genuinely
    /// conflict (same `seq`, same `prev_hash`, different `event_hash`).
    /// The identity is forked: no winner is picked, and every operation
    /// that depends on knowing which key currently controls the identity
    /// must freeze until the owner resolves it through social recovery
    /// (see `docs/projects/backend-server/architecture/identity.md`'s
    /// "Social recovery via M-of-N guardians" section) — recovery already
    /// authenticates a *new* credential through guardian approval plus a
    /// public delay, exactly the kind of independent, out-of-band
    /// resolution a forked chain needs, so this module reuses that path
    /// rather than inventing a second one.
    Forked { candidates: Vec<&'a ChainedEvent> },
}

/// Resolves a single position in an identity's chain given every event
/// that validly extends the same predecessor (same `seq`, same
/// `prev_hash`) — the definition of "concurrent" this module uses. Callers
/// only ever hand this function events that already passed that filter;
/// see [`IdentityChain::apply`] for the full per-chain algorithm including
/// that filtering step.
///
/// Rule, in priority order:
/// 1. Any [`ActionClass::ChainCritical`] candidate present alongside
///    another candidate (critical or not) forks the identity — a
///    concurrent edit can never be allowed to race a key rotation/
///    recovery/revocation and "win" by timestamp.
/// 2. [`ActionClass::Monotonic`] beats [`ActionClass::OrdinaryEdit`]
///    unconditionally, regardless of timestamp — a revocation is never
///    undone by an older or merely-concurrent edit.
/// 3. Among same-class candidates, the later (already-clamped) timestamp
///    wins; a tie is broken by the smaller `event_hash`, so the outcome
///    never depends on arrival order, only on the events' own content.
pub fn resolve_conflict<'a>(candidates: &[&'a ChainedEvent]) -> ConflictResolution<'a> {
    assert!(
        !candidates.is_empty(),
        "resolve_conflict requires at least one candidate"
    );
    if candidates.len() == 1 {
        return ConflictResolution::Resolved(candidates[0]);
    }

    if candidates
        .iter()
        .any(|c| c.class == ActionClass::ChainCritical)
    {
        return ConflictResolution::Forked {
            candidates: candidates.to_vec(),
        };
    }

    let highest_class_present = if candidates.iter().any(|c| c.class == ActionClass::Monotonic) {
        ActionClass::Monotonic
    } else {
        ActionClass::OrdinaryEdit
    };

    let mut in_class: Vec<&ChainedEvent> = candidates
        .iter()
        .copied()
        .filter(|c| c.class == highest_class_present)
        .collect();

    // Later timestamp first, tie broken by the smaller hash — a total
    // order over the tied candidates so the winner is always the same
    // element regardless of the slice's original order.
    in_class.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| a.event_hash.cmp(&b.event_hash))
    });

    ConflictResolution::Resolved(in_class[0])
}

/// Outcome of replaying a full set of events for one identity's chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainOutcome {
    /// The winning event at each `seq` the chain actually reached,
    /// in order. The last entry is the chain's current head.
    pub accepted: Vec<ChainedEvent>,
    /// Set once a [`ConflictResolution::Forked`] occurs; `accepted` stops
    /// at the last unforked `seq`. `None` while the identity is healthy.
    pub forked_at: Option<u64>,
}

impl ChainOutcome {
    pub fn is_forked(&self) -> bool {
        self.forked_at.is_some()
    }

    pub fn head(&self) -> Option<&ChainedEvent> {
        self.accepted.last()
    }
}

/// Replays an unordered set of events belonging to one identity and
/// produces the single deterministic [`ChainOutcome`] every honest node
/// must reach from that same set, independent of arrival order.
///
/// Algorithm: starting from the genesis predecessor (`seq = 1`, no
/// `prev_hash`), repeatedly gather every event whose `seq`/`prev_hash`
/// validly extends the current head, resolve that group with
/// [`resolve_conflict`], and advance. An event whose `prev_hash` doesn't
/// match the actual current head (for example, one built on a since-
/// superseded losing branch) is simply not a candidate at that step — it
/// never becomes part of current state, but it is not an error either;
/// the caller's history/audit trail still has it.
pub fn apply_chain(mut events: Vec<ChainedEvent>) -> ChainOutcome {
    let mut accepted = Vec::new();
    let mut head_hash: Option<EventHash> = None;
    let mut next_seq: u64 = 1;

    loop {
        let candidates: Vec<ChainedEvent> = {
            let mut matched = Vec::new();
            let mut rest = Vec::new();
            for event in events.into_iter() {
                if event.seq == next_seq && event.prev_hash == head_hash {
                    matched.push(event);
                } else {
                    rest.push(event);
                }
            }
            events = rest;
            matched
        };

        if candidates.is_empty() {
            break;
        }

        let refs: Vec<&ChainedEvent> = candidates.iter().collect();
        match resolve_conflict(&refs) {
            ConflictResolution::Resolved(winner) => {
                head_hash = Some(winner.event_hash);
                accepted.push(winner.clone());
                next_seq += 1;
            }
            ConflictResolution::Forked { .. } => {
                return ChainOutcome {
                    accepted,
                    forked_at: Some(next_seq),
                };
            }
        }
    }

    ChainOutcome {
        accepted,
        forked_at: None,
    }
}

/// Deterministically ordered view used only to make test fixtures easy to
/// build from a `(seq, [events])` map — not used by [`apply_chain`]
/// itself, which works from a flat `Vec` and does its own bucketing.
pub fn events_by_seq(events: &[ChainedEvent]) -> BTreeMap<u64, Vec<&ChainedEvent>> {
    let mut map: BTreeMap<u64, Vec<&ChainedEvent>> = BTreeMap::new();
    for event in events {
        map.entry(event.seq).or_default().push(event);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hash_of(label: &str) -> EventHash {
        let mut hasher = Sha256::new();
        hasher.update(label.as_bytes());
        hasher.finalize().into()
    }

    fn t(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(seconds)
    }

    fn ordinary(seq: u64, prev: Option<EventHash>, hash: &str, ts: i64) -> ChainedEvent {
        ChainedEvent {
            seq,
            prev_hash: prev,
            event_hash: hash_of(hash),
            timestamp: t(ts),
            class: ActionClass::OrdinaryEdit,
            authority: EventAuthority::AuthenticatedSession,
        }
    }

    fn monotonic(seq: u64, prev: Option<EventHash>, hash: &str, ts: i64) -> ChainedEvent {
        ChainedEvent {
            class: ActionClass::Monotonic,
            ..ordinary(seq, prev, hash, ts)
        }
    }

    fn critical(seq: u64, prev: Option<EventHash>, hash: &str, ts: i64) -> ChainedEvent {
        ChainedEvent {
            class: ActionClass::ChainCritical,
            authority: EventAuthority::OwnerSigned,
            ..ordinary(seq, prev, hash, ts)
        }
    }

    // --- compute_event_hash -------------------------------------------

    fn chain_input<'a>(id: &'a IdentityId, payload: &'a [u8; 32]) -> ChainHashInput<'a> {
        ChainHashInput {
            identity_id: id,
            seq: 2,
            prev_hash: None,
            event_id: Uuid::from_u128(7),
            kind: "profile.updated",
            issuer: "i:1",
            subject: "i:1",
            event_version: 1,
            timestamp_micros: 1_000,
            payload_hash: payload,
            envelope: &ENVELOPE,
        }
    }

    static ENVELOPE: std::sync::LazyLock<Envelope> =
        std::sync::LazyLock::new(|| Envelope::current(tags::IDENTITY_CHAIN_EVENT));

    #[test]
    fn compute_event_hash_is_deterministic_for_identical_inputs() {
        let (id, p) = (IdentityId::random_for_tests(), [3u8; 32]);
        assert_eq!(
            compute_event_hash(&chain_input(&id, &p)),
            compute_event_hash(&chain_input(&id, &p))
        );
    }

    #[test]
    fn compute_event_hash_differs_when_any_field_differs() {
        let (id, p) = (IdentityId::random_for_tests(), [3u8; 32]);
        let other_id = IdentityId::random_for_tests();
        let (other_p, prev) = ([4u8; 32], [5u8; 32]);
        let base = compute_event_hash(&chain_input(&id, &p)).unwrap();
        let ext_envelope = Envelope {
            extensions: crate::signing_bytes::Extensions::new(
                vec![crate::signing_bytes::Extension {
                    ext_type: 9,
                    critical: false,
                    value: vec![1],
                }],
                1,
            )
            .unwrap(),
            ..ENVELOPE.clone()
        };
        let variants = [
            ChainHashInput {
                identity_id: &other_id,
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                seq: 3,
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                prev_hash: Some(&prev),
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                event_id: Uuid::from_u128(8),
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                kind: "profile.x",
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                issuer: "i:2",
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                subject: "i:2",
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                event_version: 2,
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                timestamp_micros: 1_001,
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                payload_hash: &other_p,
                ..chain_input(&id, &p)
            },
            ChainHashInput {
                envelope: &ext_envelope,
                ..chain_input(&id, &p)
            },
        ];
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(compute_event_hash(v).unwrap(), base, "variant {i}");
        }
    }

    #[test]
    fn chain_event_bytes_start_with_the_tag_and_layout_version() {
        let (id, p) = (IdentityId::random_for_tests(), [3u8; 32]);
        let bytes = chain_event_signing_bytes(&chain_input(&id, &p)).unwrap();
        assert!(bytes.starts_with(b"avalon.identity.chain_event\x00\x01\x00\x00\x00\x01"));
    }

    // --- clamp_timestamp -------------------------------------------------

    #[test]
    fn clamp_timestamp_accepts_a_value_within_bounds() {
        let bounds = ClockSkewBounds::default();
        let now = t(10_000);
        assert_eq!(clamp_timestamp(t(9_999), now, &bounds), Ok(t(9_999)));
        assert_eq!(
            clamp_timestamp(t(10_000) + Duration::seconds(60), now, &bounds),
            Ok(t(10_000) + Duration::seconds(60))
        );
    }

    #[test]
    fn clamp_timestamp_rejects_a_forged_far_future_value() {
        let bounds = ClockSkewBounds::default();
        let now = t(10_000);
        let forged = now + Duration::days(3650);
        assert_eq!(
            clamp_timestamp(forged, now, &bounds),
            Err(TimestampError::TooFarInFuture)
        );
    }

    #[test]
    fn clamp_timestamp_enforces_an_optional_past_bound() {
        let bounds = ClockSkewBounds {
            max_future_skew: Duration::minutes(5),
            max_past_skew: Some(Duration::minutes(5)),
        };
        let now = t(10_000);
        assert_eq!(
            clamp_timestamp(now - Duration::hours(1), now, &bounds),
            Err(TimestampError::TooFarInPast)
        );
    }

    // --- resolve_conflict --------------------------------------------

    #[test]
    fn ordinary_conflict_is_won_by_the_later_timestamp() {
        let a = ordinary(2, None, "a", 100);
        let b = ordinary(2, None, "b", 200);
        let resolved = resolve_conflict(&[&a, &b]);
        assert_eq!(resolved, ConflictResolution::Resolved(&b));
    }

    #[test]
    fn ordinary_conflict_result_is_independent_of_argument_order() {
        let a = ordinary(2, None, "a", 100);
        let b = ordinary(2, None, "b", 200);
        assert_eq!(resolve_conflict(&[&a, &b]), resolve_conflict(&[&b, &a]));
    }

    #[test]
    fn ordinary_conflict_tie_is_broken_by_the_smaller_hash() {
        let a = ordinary(2, None, "aaa", 100);
        let b = ordinary(2, None, "zzz", 100);
        let expected = if a.event_hash < b.event_hash { &a } else { &b };
        assert_eq!(
            resolve_conflict(&[&a, &b]),
            ConflictResolution::Resolved(expected)
        );
        // Order-independence holds for the tie-break path too.
        assert_eq!(resolve_conflict(&[&a, &b]), resolve_conflict(&[&b, &a]));
    }

    #[test]
    fn monotonic_beats_ordinary_even_with_an_earlier_timestamp() {
        let edit = ordinary(2, None, "edit", 500);
        let revoke = monotonic(2, None, "revoke", 100);
        assert_eq!(
            resolve_conflict(&[&edit, &revoke]),
            ConflictResolution::Resolved(&revoke)
        );
    }

    #[test]
    fn two_chain_critical_candidates_fork_the_identity() {
        let rotate_a = critical(2, None, "rotate-a", 100);
        let rotate_b = critical(2, None, "rotate-b", 100);
        let outcome = resolve_conflict(&[&rotate_a, &rotate_b]);
        match outcome {
            ConflictResolution::Forked { candidates } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Forked, got {other:?}"),
        }
    }

    #[test]
    fn a_chain_critical_candidate_forks_even_against_an_ordinary_edit() {
        let rotate = critical(2, None, "rotate", 100);
        let edit = ordinary(2, None, "edit", 999_999);
        let outcome = resolve_conflict(&[&rotate, &edit]);
        assert!(matches!(outcome, ConflictResolution::Forked { .. }));
    }

    // --- apply_chain: full chain replay -------------------------------

    #[test]
    fn two_nodes_converge_on_the_same_value_regardless_of_arrival_order() {
        let genesis = ordinary(1, None, "genesis", 0);
        let genesis_hash = genesis.event_hash;
        let edit_a = ordinary(2, Some(genesis_hash), "bio=hello", 100);
        let edit_b = ordinary(2, Some(genesis_hash), "bio=world", 200);

        let node_1_order = vec![genesis.clone(), edit_a.clone(), edit_b.clone()];
        let node_2_order = vec![edit_b.clone(), genesis.clone(), edit_a.clone()];

        let outcome_1 = apply_chain(node_1_order);
        let outcome_2 = apply_chain(node_2_order);

        assert_eq!(outcome_1, outcome_2);
        assert_eq!(outcome_1.head().unwrap().event_hash, edit_b.event_hash);
    }

    #[test]
    fn out_of_order_arrival_still_converges_to_the_full_chain() {
        let genesis = ordinary(1, None, "genesis", 0);
        let step_2 = ordinary(2, Some(genesis.event_hash), "step2", 10);
        let step_3 = ordinary(3, Some(step_2.event_hash), "step3", 20);

        // Deliberately reversed arrival order.
        let outcome = apply_chain(vec![step_3.clone(), step_2.clone(), genesis.clone()]);

        assert_eq!(outcome.accepted.len(), 3);
        assert_eq!(outcome.head().unwrap().event_hash, step_3.event_hash);
        assert!(!outcome.is_forked());
    }

    #[test]
    fn concurrent_revocation_and_edit_the_revocation_wins() {
        let genesis = ordinary(1, None, "genesis", 0);
        let edit = ordinary(2, Some(genesis.event_hash), "edit", 500);
        let revoke = monotonic(2, Some(genesis.event_hash), "revoke", 50);

        let outcome = apply_chain(vec![genesis, edit, revoke.clone()]);

        assert_eq!(outcome.head().unwrap().event_hash, revoke.event_hash);
        assert!(!outcome.is_forked());
    }

    #[test]
    fn a_forked_rotation_is_detected_and_freezes_the_chain() {
        let genesis = ordinary(1, None, "genesis", 0);
        let rotate_a = critical(2, Some(genesis.event_hash), "rotate-a", 100);
        let rotate_b = critical(2, Some(genesis.event_hash), "rotate-b", 100);

        let outcome = apply_chain(vec![genesis.clone(), rotate_a, rotate_b]);

        assert!(outcome.is_forked());
        assert_eq!(outcome.forked_at, Some(2));
        // The chain froze at genesis — nothing past the fork point is
        // accepted into current state.
        assert_eq!(outcome.accepted.len(), 1);
        assert_eq!(outcome.head().unwrap().event_hash, genesis.event_hash);
    }

    #[test]
    fn an_event_built_on_a_superseded_branch_never_joins_current_state() {
        let genesis = ordinary(1, None, "genesis", 0);
        let edit_a = ordinary(2, Some(genesis.event_hash), "a", 100);
        let edit_b = ordinary(2, Some(genesis.event_hash), "b", 200); // wins over edit_a
                                                                      // Built on top of the losing branch (edit_a) — never a valid
                                                                      // continuation of the chain that actually got accepted.
        let orphan = ordinary(3, Some(edit_a.event_hash), "orphan", 300);

        let outcome = apply_chain(vec![genesis, edit_a, edit_b.clone(), orphan]);

        assert_eq!(outcome.accepted.len(), 2);
        assert_eq!(outcome.head().unwrap().event_hash, edit_b.event_hash);
        assert!(!outcome.is_forked());
    }

    #[test]
    fn three_way_ordinary_conflict_still_has_one_deterministic_winner() {
        let genesis = ordinary(1, None, "genesis", 0);
        let a = ordinary(2, Some(genesis.event_hash), "a", 100);
        let b = ordinary(2, Some(genesis.event_hash), "b", 300);
        let c = ordinary(2, Some(genesis.event_hash), "c", 200);

        let outcome_1 = apply_chain(vec![genesis.clone(), a.clone(), b.clone(), c.clone()]);
        let outcome_2 = apply_chain(vec![c, genesis, b.clone(), a]);

        assert_eq!(outcome_1, outcome_2);
        assert_eq!(outcome_1.head().unwrap().event_hash, b.event_hash);
    }

    // --- ActionClass::classify ----------------------------------------

    #[test]
    fn profile_updated_classifies_as_ordinary_edit() {
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Known(
                ProtocolEventKindVariant::ProfileUpdated
            )),
            Some(ActionClass::OrdinaryEdit)
        );
    }

    #[test]
    fn signing_key_events_classify_as_chain_critical() {
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Known(
                ProtocolEventKindVariant::IdentitySigningKeyAdded
            )),
            Some(ActionClass::ChainCritical)
        );
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Known(
                ProtocolEventKindVariant::IdentitySigningKeyRevoked
            )),
            Some(ActionClass::ChainCritical)
        );
    }

    #[test]
    fn rollback_reversal_events_classify_as_monotonic() {
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Known(
                ProtocolEventKindVariant::FriendRelationshipReversed
            )),
            Some(ActionClass::Monotonic)
        );
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Known(
                ProtocolEventKindVariant::GuildMembershipReversed
            )),
            Some(ActionClass::Monotonic)
        );
    }

    #[test]
    fn a_kind_outside_the_layer_1_chain_classifies_as_none() {
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Known(
                ProtocolEventKindVariant::AchievementIssued
            )),
            None
        );
        assert_eq!(
            ActionClass::classify(&ProtocolEventKind::Other("some.future.kind".into())),
            None
        );
    }
}
