//! The ledger's hash-chain math — deliberately storage-agnostic (issue
//! #178), shared verbatim by every `SettlementProvider` implementation
//! (`postgres.rs`, `rocksdb_backend.rs`). Given the same prior hash and the
//! same event content, every backend must compute the exact same
//! `entry_hash`/batch root — that's what makes "the ledger" a single
//! well-defined thing independent of which engine happens to be storing it
//! today, and it's the whole point of duplicating nothing here.

use avalon_protocol::events::ProtocolEvent;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// All-zero hash, the `prev_hash` of the very first entry in the chain.
pub(crate) const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// The fields that make up an entry's content hash — grouped so recomputing
/// a hash (at insert time from a `ProtocolEvent`, or at verify time from a
/// stored row) takes one argument, not eight.
pub(crate) struct EntryContent<'a> {
    pub(crate) event_id: Uuid,
    pub(crate) kind: &'a str,
    pub(crate) issuer: &'a str,
    pub(crate) subject: &'a str,
    pub(crate) payload: &'a serde_json::Value,
    pub(crate) timestamp: time::OffsetDateTime,
    pub(crate) version: i32,
}

/// Serializes `value` with object keys sorted, recursively, so the result
/// is independent of the `Value`'s in-memory map ordering.
///
/// This matters because that ordering is *not* stable across an entry's own
/// lifecycle: a payload is built once in-process (order depends on whether
/// `serde_json`'s `preserve_order` feature is active in whichever binary
/// links this crate in), then travels through the outbox's storage and the
/// ledger's own storage before `avalon inspect-ledger(-full)` ever reads it
/// back to verify — and neither Postgres's `jsonb` type nor a RocksDB byte
/// blob preserves original key order or formatting; each backend re-emits
/// or stores keys in its own way. Hashing `Value::to_string()` directly, as
/// this used to, made the hash depend on which of those orderings happened
/// to be current at the moment of hashing rather than on the payload's
/// actual content, producing false "broken chain" reports for any
/// multi-key payload despite nothing being tampered with. Sorting keys
/// ourselves removes the dependency on any of those orderings agreeing
/// with each other.
pub(crate) fn canonical_json(value: &serde_json::Value) -> String {
    fn write(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).expect("string always serializes"));
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            leaf => out.push_str(&leaf.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

pub(crate) fn hash_entry(prev_hash: &str, content: &EntryContent<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(content.event_id.as_bytes());
    hasher.update(content.kind.as_bytes());
    hasher.update(content.issuer.as_bytes());
    hasher.update(content.subject.as_bytes());
    hasher.update(canonical_json(content.payload).as_bytes());
    hasher.update(content.timestamp.unix_timestamp().to_le_bytes());
    hasher.update(content.version.to_le_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn hash_event(prev_hash: &str, event: &ProtocolEvent) -> String {
    hash_entry(
        prev_hash,
        &EntryContent {
            event_id: event.id,
            kind: &event.kind,
            issuer: event.issuer.as_str(),
            subject: event.subject.as_str(),
            payload: &event.payload,
            timestamp: event.timestamp,
            version: event.version as i32,
        },
    )
}

/// Recomputes a batch's root the same way `commit` produced it in the first
/// place: replay the hash chain across the batch's own entries, in order,
/// starting from the hash the batch extended (its first entry's stored
/// `prev_hash` — the ledger's tip immediately before this batch was
/// committed, not verified here since that's a cross-batch concern, not
/// this batch's own integrity).
///
/// This is a pure, storage-free function on purpose (issue #38's acceptance
/// criteria that `verify` "recomputes and compares the batch root, not just
/// row existence") — it recomputes from each entry's actual stored content,
/// so tampering with any entry's content in the batch (not just deleting a
/// row) changes the result. Each backend's own `verify` is the only real
/// caller; kept free-standing so it's directly unit-testable without any
/// storage engine at all.
pub(crate) fn recompute_batch_root(
    entering_prev_hash: &str,
    entries: &[EntryContent<'_>],
) -> String {
    let mut prev = entering_prev_hash.to_string();
    for content in entries {
        prev = hash_entry(&prev, content);
    }
    prev
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_is_stable_across_key_order() {
        let a = json!({ "from": "x", "to": "y", "actor": "x" });
        let b = json!({ "actor": "x", "to": "y", "from": "x" });
        let c = json!({ "to": "y", "from": "x", "actor": "x" });
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&b), canonical_json(&c));
    }

    #[test]
    fn canonical_json_sorts_nested_objects_too() {
        let a = json!({ "outer": { "z": 1, "a": 2 } });
        let b = json!({ "outer": { "a": 2, "z": 1 } });
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn canonical_json_still_distinguishes_different_content() {
        let a = json!({ "from": "x", "to": "y" });
        let b = json!({ "from": "x", "to": "z" });
        assert_ne!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn hash_entry_is_order_independent_for_the_full_entry() {
        let event_id = Uuid::new_v4();
        let timestamp = time::OffsetDateTime::now_utc();
        let a = json!({ "from": "x", "to": "y", "actor": "x" });
        let b = json!({ "actor": "x", "to": "y", "from": "x" });

        let hash_a = hash_entry(
            GENESIS_HASH,
            &EntryContent {
                event_id,
                kind: "friend.requested",
                issuer: "identity:x:self:friend_requested",
                subject: "identity:y:self:friend_requested",
                payload: &a,
                timestamp,
                version: 1,
            },
        );
        let hash_b = hash_entry(
            GENESIS_HASH,
            &EntryContent {
                event_id,
                kind: "friend.requested",
                issuer: "identity:x:self:friend_requested",
                subject: "identity:y:self:friend_requested",
                payload: &b,
                timestamp,
                version: 1,
            },
        );
        assert_eq!(hash_a, hash_b);
    }

    fn sample_entry(event_id: Uuid, payload: &serde_json::Value) -> EntryContent<'_> {
        EntryContent {
            event_id,
            kind: "guild.created",
            issuer: "identity:x:self:guild_created",
            subject: "guild:y:self:guild_created",
            payload,
            timestamp: time::OffsetDateTime::UNIX_EPOCH,
            version: 1,
        }
    }

    #[test]
    fn recompute_batch_root_matches_sequential_hash_entry_calls() {
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let payloads = [json!({"a": 1}), json!({"b": 2}), json!({"c": 3})];
        let entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();

        let expected = {
            let mut prev = GENESIS_HASH.to_string();
            for entry in &entries {
                prev = hash_entry(&prev, entry);
            }
            prev
        };

        assert_eq!(recompute_batch_root(GENESIS_HASH, &entries), expected);
    }

    #[test]
    fn recompute_batch_root_detects_tampering_with_any_entry_in_the_batch() {
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let original_payloads = [json!({"a": 1}), json!({"b": 2}), json!({"c": 3})];
        let entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&original_payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();
        let root = recompute_batch_root(GENESIS_HASH, &entries);

        // Tamper with the *first* entry's payload — not the last — to prove
        // this isn't just re-hashing the tip; it must actually replay the
        // whole chain to notice.
        let mut tampered_payloads = original_payloads.clone();
        tampered_payloads[0] = json!({"a": 999});
        let tampered_entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&tampered_payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();
        let tampered_root = recompute_batch_root(GENESIS_HASH, &tampered_entries);

        assert_ne!(root, tampered_root);
    }

    #[test]
    fn recompute_batch_root_of_a_single_event_batch_is_legal() {
        let payload = json!({"solo": true});
        let entries = vec![sample_entry(Uuid::new_v4(), &payload)];
        let root = recompute_batch_root(GENESIS_HASH, &entries);
        assert_eq!(root, hash_entry(GENESIS_HASH, &entries[0]));
    }
}
