//! Revocation is final: once a revoking event of an identity is on the ledger, no event signed by the
//! revoked key counts unless it is the revoking event's own history or was on the ledger strictly
//! earlier. The order is the witness-signed anchor time of each event's ledger entry, never a node's
//! clock or an event's own time. Pure logic over the stored set of an identity's events, so every node
//! with the same events and anchors reaches the same verdict whatever order they arrived in.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::events::ProtocolEvent;
use crate::identity_chain::EventHash;
use crate::identity_chain_wire::parse_hash;

/// A revoking event and the event it makes inadmissible.
pub type Conflict = (EventHash, EventHash);

/// What a revoking event ends.
enum Revokes {
    /// One key (`identity.signing_key_revoked`).
    Key(Uuid),
    /// Every key but this one (`identity.recovered`, whose new key is the exception).
    AllBut(Uuid),
}

fn uuid_at(payload: &Value, field: &str) -> Option<Uuid> {
    serde_json::from_value(payload.get(field)?.clone()).ok()
}

/// The key `event` was signed with: the revoker or approver of an owner-signed key event, the new key
/// of a recovery, otherwise the key its chain position names.
pub fn signing_key_of(event: &ProtocolEvent) -> Option<Uuid> {
    match event.kind.as_str() {
        "identity.signing_key_added" => uuid_at(&event.payload, "approved_by_signing_key_id"),
        "identity.signing_key_revoked" => uuid_at(&event.payload, "revoked_by_signing_key_id"),
        "identity.recovered" => uuid_at(&event.payload, "request_id"),
        _ => event.identity_chain.as_ref()?.signing_key_id,
    }
}

fn revokes(event: &ProtocolEvent) -> Option<Revokes> {
    match event.kind.as_str() {
        "identity.signing_key_revoked" => {
            uuid_at(&event.payload, "signing_key_id").map(Revokes::Key)
        }
        "identity.recovered" => uuid_at(&event.payload, "request_id").map(Revokes::AllBut),
        _ => None,
    }
}

fn prev_of(event: &ProtocolEvent) -> Option<EventHash> {
    event
        .identity_chain
        .as_ref()?
        .prev_hash
        .as_deref()
        .and_then(parse_hash)
}

/// Every pair of a revoking event and an event signed by a key it revokes that is not on the revoking
/// event's own line (neither its ancestor nor its descendant): the revoker's own history and its
/// descendants are judged by chain position alone.
pub fn revocation_conflicts(events: &[(ProtocolEvent, EventHash)]) -> Vec<Conflict> {
    let all: BTreeSet<EventHash> = events.iter().map(|(_, hash)| *hash).collect();
    conflicts_among(events, &all)
}

/// [`revocation_conflicts`] between the events in `active`, with ancestry read from all of `events`.
fn conflicts_among(
    events: &[(ProtocolEvent, EventHash)],
    active: &BTreeSet<EventHash>,
) -> Vec<Conflict> {
    let parents: BTreeMap<EventHash, Option<EventHash>> = events
        .iter()
        .map(|(event, hash)| (*hash, prev_of(event)))
        .collect();
    let ancestors_of = |start: &EventHash| -> BTreeSet<EventHash> {
        let mut out = BTreeSet::new();
        let mut at = parents.get(start).copied().flatten();
        while let Some(hash) = at {
            if !out.insert(hash) {
                break;
            }
            at = parents.get(&hash).copied().flatten();
        }
        out
    };
    let ancestors: BTreeMap<EventHash, BTreeSet<EventHash>> = events
        .iter()
        .map(|(_, hash)| (*hash, ancestors_of(hash)))
        .collect();
    let mut out = Vec::new();
    for (revoker, revoker_hash) in events.iter().filter(|(_, h)| active.contains(h)) {
        let Some(revokes) = revokes(revoker) else {
            continue;
        };
        for (victim, victim_hash) in events.iter().filter(|(_, h)| active.contains(h)) {
            if victim_hash == revoker_hash || ancestors[revoker_hash].contains(victim_hash) {
                continue;
            }
            let Some(key) = signing_key_of(victim) else {
                continue;
            };
            let hit = match revokes {
                Revokes::Key(revoked) => key == revoked,
                Revokes::AllBut(spared) => key != spared,
            };
            if hit && !ancestors[victim_hash].contains(revoker_hash) {
                out.push((*revoker_hash, *victim_hash));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// What the revocation rule decided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Judged {
    /// Anchored no earlier than a revocation of their key.
    pub superseded: BTreeSet<EventHash>,
    /// Beside a dated revocation but not dated themselves: kept out of the chain until a cosigned
    /// head dates them.
    pub pending: BTreeSet<EventHash>,
    /// Beside a revocation that is not dated yet. An undated revocation has no standing, so these stay
    /// in the chain, but a node should not take them in until the revocation is dated.
    pub awaiting_revocation: BTreeSet<EventHash>,
}

impl Judged {
    /// Whether the chain leaves the event out.
    pub fn is_excluded(&self, hash: &EventHash) -> bool {
        self.superseded.contains(hash) || self.pending.contains(hash)
    }
}

/// Judges `conflicts` given the anchor time of every event that has one. Events are ordered by
/// (anchor, hash); a revocation that is itself superseded revokes nothing, and one that is not dated
/// yet revokes nothing for now.
pub fn judge_conflicts(
    conflicts: &[Conflict],
    anchors: &BTreeMap<EventHash, OffsetDateTime>,
) -> Judged {
    let involved: BTreeSet<EventHash> = conflicts.iter().flat_map(|(r, v)| [*r, *v]).collect();
    let rank = |hash: &EventHash| anchors.get(hash).map(|at| (*at, *hash));
    let mut known: Vec<EventHash> = involved
        .iter()
        .copied()
        .filter(|h| anchors.contains_key(h))
        .collect();
    known.sort_by_key(|h| rank(h));

    let mut superseded = BTreeSet::new();
    for victim in known {
        let beaten = conflicts.iter().any(|(revoker, v)| {
            *v == victim
                && !superseded.contains(revoker)
                && matches!((rank(revoker), rank(&victim)), (Some(r), Some(v)) if r < v)
        });
        if beaten {
            superseded.insert(victim);
        }
    }
    let mut pending = BTreeSet::new();
    let mut awaiting_revocation = BTreeSet::new();
    for (revoker, victim) in conflicts {
        if superseded.contains(revoker) || superseded.contains(victim) {
            continue;
        }
        match (anchors.contains_key(revoker), anchors.contains_key(victim)) {
            (true, false) => {
                pending.insert(*victim);
            }
            (false, _) => {
                awaiting_revocation.insert(*victim);
            }
            (true, true) => {}
        }
    }
    Judged {
        superseded,
        pending,
        awaiting_revocation,
    }
}

/// What keeps stored events out of the chain: recoveries the owner vetoed, and what the revocation rule
/// supersedes or holds pending.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Settled {
    pub vetoed: BTreeSet<EventHash>,
    pub judged: Judged,
}

impl Settled {
    /// Whether the chain keeps the event.
    pub fn keeps(&self, hash: &EventHash) -> bool {
        !self.vetoed.contains(hash) && !self.judged.is_excluded(hash)
    }

    /// Every event the chain leaves out.
    pub fn excluded(&self) -> BTreeSet<EventHash> {
        self.vetoed
            .iter()
            .chain(&self.judged.superseded)
            .chain(&self.judged.pending)
            .copied()
            .collect()
    }
}

/// Decides which of `events` stand, to a fixed point: a vetoed recovery revokes nothing, and a cancel
/// that is superseded vetoes nothing. Each pass changes the vetoed set or ends, so `events.len() + 1`
/// passes always settle; `settle_within` exposes the bound for tests.
pub fn settle(
    events: &[(ProtocolEvent, EventHash)],
    anchors: &BTreeMap<EventHash, OffsetDateTime>,
) -> Settled {
    settle_within(events, anchors, events.len() + 1)
}

pub fn settle_within(
    events: &[(ProtocolEvent, EventHash)],
    anchors: &BTreeMap<EventHash, OffsetDateTime>,
    rounds: usize,
) -> Settled {
    let mut vetoed = BTreeSet::new();
    let mut judged = Judged::default();
    for _ in 0..rounds {
        let base: BTreeSet<EventHash> = events
            .iter()
            .map(|(_, hash)| *hash)
            .filter(|hash| !vetoed.contains(hash))
            .collect();
        judged = judge_conflicts(&conflicts_among(events, &base), anchors);
        let standing: Vec<(ProtocolEvent, EventHash)> = events
            .iter()
            .filter(|(_, hash)| !judged.is_excluded(hash))
            .cloned()
            .collect();
        let next = crate::identity_chain_wire::vetoed_recoveries(&standing);
        if next == vetoed {
            break;
        }
        vetoed = next;
    }
    Settled { vetoed, judged }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::IdentityChainPosition;
    use crate::identity_chain_wire::event_hash;
    use crate::identity_id::IdentityId;
    use serde_json::json;
    use time::Duration;

    const T: Uuid = Uuid::from_u128(0xdead);
    const OWNER_KEY: Uuid = Uuid::from_u128(0xa11ce);

    fn at(hours: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::hours(hours)
    }

    struct Chain(IdentityId);

    impl Chain {
        fn event(
            &self,
            kind: &str,
            seq: u64,
            prev: Option<&EventHash>,
            key: Uuid,
            payload: Value,
        ) -> (ProtocolEvent, EventHash) {
            let gid = crate::ids::GlobalId::new("identity", &self.0.to_string(), "self", "x");
            let mut position = IdentityChainPosition::current(seq, prev.map(hex::encode));
            position.signing_key_id = Some(key);
            let event = ProtocolEvent {
                id: Uuid::new_v4(),
                kind: kind.to_string(),
                issuer: gid.clone(),
                subject: gid,
                payload,
                timestamp: OffsetDateTime::UNIX_EPOCH,
                version: 1,
                identity_chain: Some(position),
            };
            let hash = event_hash(&event).unwrap();
            (event, hash)
        }

        fn profile(
            &self,
            seq: u64,
            prev: Option<&EventHash>,
            key: Uuid,
            tag: u32,
        ) -> (ProtocolEvent, EventHash) {
            self.event("profile.updated", seq, prev, key, json!({ "tag": tag }))
        }

        fn revocation(
            &self,
            seq: u64,
            prev: Option<&EventHash>,
            revoked: Uuid,
            by: Uuid,
        ) -> (ProtocolEvent, EventHash) {
            let payload = json!({
                "identity_id": self.0,
                "signing_key_id": revoked,
                "revoked_by_signing_key_id": by,
                "signature": base64_zeros(),
            });
            self.event("identity.signing_key_revoked", seq, prev, by, payload)
        }
    }

    fn base64_zeros() -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([0u8; 64])
    }

    #[test]
    fn an_event_beside_a_revocation_of_its_key_is_a_conflict_but_the_revokers_own_line_is_not() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let e2 = chain.profile(2, Some(&e1.1), T, 2);
        let revocation = chain.revocation(3, Some(&e2.1), T, OWNER_KEY);
        // The thief's event at the same position as the revocation, and one at old position 2.
        let thief_3 = chain.profile(3, Some(&e2.1), T, 30);
        let thief_2 = chain.profile(2, Some(&e1.1), T, 20);
        let all = [
            e1,
            e2.clone(),
            revocation.clone(),
            thief_3.clone(),
            thief_2.clone(),
        ];
        let conflicts = revocation_conflicts(&all);
        assert_eq!(
            conflicts,
            {
                let mut v = vec![(revocation.1, thief_3.1), (revocation.1, thief_2.1)];
                v.sort();
                v
            },
            "e2 is the revocation's ancestor and not a conflict"
        );
    }

    #[test]
    fn a_descendant_of_a_revocation_is_judged_by_position_alone() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let revocation = chain.revocation(2, Some(&e1.1), T, OWNER_KEY);
        let after = chain.profile(3, Some(&revocation.1), T, 3);
        assert!(revocation_conflicts(&[e1, revocation, after]).is_empty());
    }

    #[test]
    fn only_events_anchored_strictly_before_the_revocation_survive_it() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let revocation = chain.revocation(2, Some(&e1.1), T, OWNER_KEY);
        let early = chain.profile(2, Some(&e1.1), T, 2);
        let late = chain.profile(2, Some(&e1.1), T, 3);
        let all = [e1, revocation.clone(), early.clone(), late.clone()];
        let conflicts = revocation_conflicts(&all);
        let anchors = BTreeMap::from([(revocation.1, at(10)), (early.1, at(9)), (late.1, at(11))]);
        let judged = judge_conflicts(&conflicts, &anchors);
        assert_eq!(judged.superseded, BTreeSet::from([late.1]));
        assert!(judged.pending.is_empty());
    }

    #[test]
    fn an_undated_event_waits_but_an_undated_revocation_has_no_standing() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let revocation = chain.revocation(2, Some(&e1.1), T, OWNER_KEY);
        let thief = chain.profile(2, Some(&e1.1), T, 2);
        let conflicts = revocation_conflicts(&[e1, revocation.clone(), thief.clone()]);
        // The revocation is dated, the event is not: the event stays out until it is dated.
        let judged = judge_conflicts(&conflicts, &BTreeMap::from([(revocation.1, at(10))]));
        assert_eq!(judged.pending, BTreeSet::from([thief.1]));
        assert!(judged.is_excluded(&thief.1));
        // The event is dated, the revocation is not: it removes nothing yet, but is waited on.
        let judged = judge_conflicts(&conflicts, &BTreeMap::from([(thief.1, at(1))]));
        assert!(!judged.is_excluded(&thief.1));
        assert_eq!(judged.awaiting_revocation, BTreeSet::from([thief.1]));
    }

    #[test]
    fn a_superseded_revocation_revokes_nothing() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        // The thief (holding T) revokes the owner's key first; the owner's revocation of T, signed
        // by the key just revoked, comes later and is superseded.
        let thief_revokes = chain.revocation(2, Some(&e1.1), OWNER_KEY, T);
        let owner_revokes = chain.revocation(2, Some(&e1.1), T, OWNER_KEY);
        let all = [e1, thief_revokes.clone(), owner_revokes.clone()];
        let conflicts = revocation_conflicts(&all);
        let anchors = BTreeMap::from([(thief_revokes.1, at(5)), (owner_revokes.1, at(6))]);
        let judged = judge_conflicts(&conflicts, &anchors);
        assert_eq!(judged.superseded, BTreeSet::from([owner_revokes.1]));
    }

    #[test]
    fn the_verdict_does_not_depend_on_the_order_the_events_are_listed_in() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let e2 = chain.profile(2, Some(&e1.1), OWNER_KEY, 2);
        let revocation = chain.revocation(3, Some(&e2.1), T, OWNER_KEY);
        let thief_a = chain.profile(2, Some(&e1.1), T, 20);
        let thief_b = chain.profile(3, Some(&e2.1), T, 30);
        let all = vec![e1, e2, revocation.clone(), thief_a.clone(), thief_b.clone()];
        let anchors = BTreeMap::from([
            (revocation.1, at(10)),
            (thief_a.1, at(12)),
            (thief_b.1, at(8)),
        ]);
        let expected = judge_conflicts(&revocation_conflicts(&all), &anchors);
        assert_eq!(expected.superseded, BTreeSet::from([thief_a.1]));
        let mut order = all.clone();
        for _ in 0..all.len() * all.len() {
            order.rotate_left(1);
            order.swap(0, 2);
            let judged = judge_conflicts(&revocation_conflicts(&order), &anchors);
            assert_eq!(judged, expected);
        }
    }

    #[test]
    fn a_chain_of_six_interdependent_revocations_settles_by_time() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let keys: Vec<Uuid> = (0..7).map(|i| Uuid::from_u128(0x100 + i)).collect();
        // r_i is signed by key i and revokes key i + 1; each is dated one hour after the last.
        let revocations: Vec<_> = (0..6)
            .map(|i| chain.revocation(2, Some(&e1.1), keys[i + 1], keys[i]))
            .collect();
        let mut all = vec![e1];
        all.extend(revocations.iter().cloned());
        let anchors: BTreeMap<_, _> = revocations
            .iter()
            .enumerate()
            .map(|(i, (_, hash))| (*hash, at(i as i64)))
            .collect();
        let settled = settle(&all, &anchors);
        // Each is superseded by its predecessor unless that one was itself superseded.
        let expected: BTreeSet<_> = [1, 3, 5].iter().map(|i| revocations[*i].1).collect();
        assert_eq!(settled.judged.superseded, expected);
        assert!(settled.judged.pending.is_empty());
    }

    #[test]
    fn the_veto_fixed_point_does_not_depend_on_a_round_bound() {
        let chain = Chain(IdentityId::random_for_tests());
        let e1 = chain.profile(1, None, OWNER_KEY, 1);
        let mut all = vec![e1.clone()];
        let mut anchors = BTreeMap::new();
        // Seven recoveries, each cancelled by an owner event the recoveries' revocations supersede.
        for level in 0..7u128 {
            let request = Uuid::from_u128(0x500 + level);
            let rec = chain.event(
                "identity.recovered",
                2,
                Some(&e1.1),
                request,
                json!({ "request_id": request, "level": level }),
            );
            let cancel = chain.event(
                "identity.recovery_cancelled",
                2,
                Some(&e1.1),
                Uuid::from_u128(0x600 + level),
                json!({ "request_id": request }),
            );
            // Higher levels are dated earlier, so each recovery supersedes the cancels below it.
            anchors.insert(rec.1, at(10 - level as i64));
            anchors.insert(cancel.1, at(50));
            all.push(rec);
            all.push(cancel);
        }
        let settled = settle(&all, &anchors);
        let reference = settle_within(&all, &anchors, 1000);
        assert_eq!(settled, reference);
        let standing: Vec<_> = all
            .iter()
            .filter(|(_, h)| !settled.judged.is_excluded(h))
            .cloned()
            .collect();
        assert_eq!(
            crate::identity_chain_wire::vetoed_recoveries(&standing),
            settled.vetoed,
            "the vetoed set is a fixed point"
        );
    }
}
