//! Known-list slot management prototype for the #934 design spike:
//! anchor reservation and per-prefix diversity caps, and refill after a
//! witness is lost. Pure logic only, sized for the spike's own tests —
//! production known-list management (persistence, real discovery
//! integration, probation, tenure-weighted selection) is #946.

/// One slot's occupant: which witness, and enough about where it came from
/// to enforce diversity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessSlot {
    pub witness_key_id: String,
    /// A coarse diversity key for this witness — an IPv4 /24, an IPv6
    /// prefix, or an operator id; the prototype treats it as an opaque
    /// string and caps how many slots share one.
    pub prefix: String,
    /// Anchor slots come from the bundled trust-anchor list
    /// (`docs/trusted-networks.json`'s `seed_nodes`) and are never evicted
    /// by ordinary refill — see the design doc's "why anchors" section.
    pub is_anchor: bool,
}

/// A bounded known list: a fixed capacity, a reserved sub-count of anchor
/// slots, and a per-prefix cap applied to every admission (anchors
/// included, so an operator can't get outsized weight by also running the
/// anchors).
pub struct KnownList {
    capacity: usize,
    anchor_capacity: usize,
    max_per_prefix: usize,
    slots: Vec<WitnessSlot>,
}

impl KnownList {
    pub fn new(capacity: usize, anchor_capacity: usize, max_per_prefix: usize) -> Self {
        assert!(
            anchor_capacity <= capacity,
            "anchor_capacity must fit within capacity"
        );
        Self {
            capacity,
            anchor_capacity,
            max_per_prefix,
            slots: Vec::new(),
        }
    }

    fn anchor_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_anchor).count()
    }

    fn prefix_count(&self, prefix: &str) -> usize {
        self.slots.iter().filter(|s| s.prefix == prefix).count()
    }

    fn contains(&self, witness_key_id: &str) -> bool {
        self.slots
            .iter()
            .any(|s| s.witness_key_id == witness_key_id)
    }

    /// Attempts to admit `candidate`. Refused (returns `false`, list
    /// unchanged) when: the witness is already present, an anchor slot
    /// would exceed `anchor_capacity`, admitting would exceed `capacity`,
    /// or `candidate.prefix` is already at `max_per_prefix` — this last
    /// check applies to anchors too, so one operator cannot claim outsized
    /// influence by also running bundled anchors.
    pub fn try_admit(&mut self, candidate: WitnessSlot) -> bool {
        if self.contains(&candidate.witness_key_id) {
            return false;
        }
        if candidate.is_anchor && self.anchor_count() >= self.anchor_capacity {
            return false;
        }
        if self.slots.len() >= self.capacity {
            return false;
        }
        if self.prefix_count(&candidate.prefix) >= self.max_per_prefix {
            return false;
        }
        self.slots.push(candidate);
        true
    }

    /// Removes a witness — models it dropping out (loss, capture,
    /// unreachability past the freshness window). Anchors can be removed
    /// too (a bundled anchor can still go offline); refill then treats the
    /// freed slot as an ordinary one, not specially anchor-reserved again
    /// until a fresh anchor is explicitly re-admitted.
    pub fn remove(&mut self, witness_key_id: &str) -> bool {
        let before = self.slots.len();
        self.slots.retain(|s| s.witness_key_id != witness_key_id);
        self.slots.len() != before
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn witness_key_ids(&self) -> Vec<&str> {
        self.slots
            .iter()
            .map(|s| s.witness_key_id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::witness::majority_threshold;

    fn slot(id: &str, prefix: &str) -> WitnessSlot {
        WitnessSlot {
            witness_key_id: id.to_string(),
            prefix: prefix.to_string(),
            is_anchor: false,
        }
    }

    fn anchor(id: &str, prefix: &str) -> WitnessSlot {
        WitnessSlot {
            witness_key_id: id.to_string(),
            prefix: prefix.to_string(),
            is_anchor: true,
        }
    }

    /// Default shape the design doc recommends: 10 slots, 2 reserved for
    /// bundled anchors, at most 2 slots per prefix.
    fn default_list() -> KnownList {
        KnownList::new(10, 2, 2)
    }

    #[test]
    fn anchors_fill_their_reserved_slots_and_no_more() {
        let mut list = default_list();
        assert!(list.try_admit(anchor("anchor-1", "anchor-net-a")));
        assert!(list.try_admit(anchor("anchor-2", "anchor-net-b")));
        // A third anchor is refused even with capacity and prefix room to
        // spare — anchor_capacity is a hard cap, not just a floor.
        assert!(!list.try_admit(anchor("anchor-3", "anchor-net-c")));
        assert_eq!(list.len(), 2);
    }

    /// Eclipse resistance: an attacker offering many candidates from one
    /// prefix cannot fill more than `max_per_prefix` slots with them, no
    /// matter how many it offers — the list stays diverse even under a
    /// flood.
    #[test]
    fn a_flood_of_same_prefix_candidates_cannot_exceed_the_diversity_cap() {
        let mut list = default_list();
        let mut admitted = 0;
        for i in 0..50 {
            if list.try_admit(slot(&format!("flood-{i}"), "attacker-net")) {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 2, "diversity cap should have refused the rest");
        assert_eq!(list.prefix_count("attacker-net"), 2);
    }

    /// Witness loss and refill: fill the list to capacity from diverse
    /// prefixes, lose several non-anchor witnesses, then refill from a
    /// fresh diverse discovery pool. The list returns to capacity, stays
    /// within the diversity cap, keeps its anchors, and
    /// `majority_threshold` — the thing that actually matters — recomputes
    /// correctly against the list's size at every step, never needing a
    /// special case for "the list just changed size."
    #[test]
    fn the_list_refills_to_capacity_after_witness_loss_and_stays_diverse() {
        let mut list = default_list();
        assert!(list.try_admit(anchor("anchor-1", "anchor-net-a")));
        assert!(list.try_admit(anchor("anchor-2", "anchor-net-b")));
        for i in 0..8 {
            assert!(list.try_admit(slot(&format!("peer-{i}"), &format!("net-{}", i % 4))));
        }
        assert_eq!(list.len(), 10);
        assert_eq!(majority_threshold(list.len()), 6);

        // Lose three non-anchor witnesses — an eclipse or a captured
        // majority-worth of nodes going dark past the freshness window.
        for i in 0..3 {
            assert!(list.remove(&format!("peer-{i}")));
        }
        assert_eq!(list.len(), 7);
        assert_eq!(
            majority_threshold(list.len()),
            4,
            "the majority rule recomputes for the shrunk list with no special case"
        );

        // Refill from a fresh, diverse discovery pool. Anchors are
        // untouched and still present throughout.
        for i in 8..14 {
            list.try_admit(slot(&format!("peer-{i}"), &format!("net-{}", i % 4)));
        }
        assert_eq!(list.len(), 10);
        assert_eq!(majority_threshold(list.len()), 6);
        assert!(list.witness_key_ids().contains(&"anchor-1"));
        assert!(list.witness_key_ids().contains(&"anchor-2"));
        for prefix in ["net-0", "net-1", "net-2", "net-3"] {
            assert!(
                list.prefix_count(prefix) <= 2,
                "diversity cap held through loss and refill for {prefix}"
            );
        }
    }

    #[test]
    fn a_witness_already_present_is_not_admitted_twice() {
        let mut list = default_list();
        assert!(list.try_admit(slot("peer-1", "net-0")));
        assert!(!list.try_admit(slot("peer-1", "net-0")));
        assert_eq!(list.len(), 1);
    }
}
