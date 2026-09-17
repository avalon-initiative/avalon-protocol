//! Exercises the mirror-recovery half of issue #316 (#300's decided
//! equivocation-response scope) against real Postgres:
//! `unresolved_equivocations`, `resolve_equivocation`, and
//! `discard_mirrored_entries_from` — the three functions #318 added but
//! left without automated coverage (its own PR body flagged this honestly
//! as remaining verification). Gated `--ignored`, same convention as
//! `crates/chain/tests/settlement.rs` and `crates/server/tests/mirror_watcher.rs`.
//!
//! Two things the ticket's own Tests section asks for, both covered here:
//! - a network with an unresolved finding stays unresolved (and therefore
//!   keeps failing the mirror-watcher's backfill gate) until an operator
//!   acts;
//! - once resolved, recovery discards only entries verified at or after the
//!   conflicting `tree_size`, never touching entries safely below it —
//!   "no data loss on the legitimate branch" for the part of the branch
//!   that was never in question.

use avalon_chain::mirror::{
    self, discard_mirrored_entries_from, resolve_equivocation, unresolved_equivocations,
    EquivocationFinding, MirroredEntry,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn unique_network_id(label: &str) -> String {
    format!("avalon-test-{label}-{}", Uuid::new_v4())
}

async fn insert_finding(
    pool: &PgPool,
    network_id: &str,
    tree_size: i64,
    root_hash_a: &str,
    root_hash_b: &str,
) {
    mirror::record_equivocation(
        pool,
        &EquivocationFinding {
            network_id: network_id.to_string(),
            tree_size,
            source_a: "peer-a".to_string(),
            root_hash_a: root_hash_a.to_string(),
            source_b: "peer-b".to_string(),
            root_hash_b: root_hash_b.to_string(),
            resolved_at: None,
            resolved_root_hash: None,
        },
    )
    .await
    .expect("record_equivocation failed");
}

fn mirrored_entry(network_id: &str, seq: i64, verified_tree_size: i64) -> MirroredEntry {
    MirroredEntry {
        source_url: "http://peer".to_string(),
        network_id: network_id.to_string(),
        seq,
        event_id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
        subject: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
        payload: Some(serde_json::json!({"note": "mirror recovery test"})),
        event_timestamp: OffsetDateTime::UNIX_EPOCH,
        version: 1,
        prev_hash: "aa".repeat(32),
        entry_hash: "bb".repeat(32),
        batch_id: Uuid::new_v4(),
        verified_tree_size,
    }
}

/// A network with an equivocation finding shows up in
/// `unresolved_equivocations` (what the mirror-watcher's backfill gate
/// checks) until `resolve_equivocation` is called for it — and resolving a
/// *different* `tree_size`'s finding, or a finding for a different
/// network, must never accidentally clear this one.
#[tokio::test]
#[ignore]
async fn a_finding_stays_unresolved_until_explicitly_resolved() {
    let pool = test_pool().await;
    let network_id = unique_network_id("stays-unresolved");
    let other_network_id = unique_network_id("other-network");

    insert_finding(&pool, &network_id, 10, &"aa".repeat(32), &"bb".repeat(32)).await;
    // A finding on an unrelated network, and one at a different tree_size
    // on the same network — neither should affect what we check below.
    insert_finding(
        &pool,
        &other_network_id,
        10,
        &"cc".repeat(32),
        &"dd".repeat(32),
    )
    .await;
    insert_finding(&pool, &network_id, 20, &"ee".repeat(32), &"ff".repeat(32)).await;

    let unresolved = unresolved_equivocations(&pool, &network_id)
        .await
        .expect("unresolved_equivocations failed");
    assert_eq!(
        unresolved.len(),
        2,
        "both findings on this network are open"
    );

    // Resolve only the tree_size=10 finding.
    let resolved_count = resolve_equivocation(&pool, &network_id, 10, &"aa".repeat(32))
        .await
        .expect("resolve_equivocation failed");
    assert_eq!(resolved_count, 1);

    let unresolved_after = unresolved_equivocations(&pool, &network_id)
        .await
        .expect("unresolved_equivocations failed");
    assert_eq!(
        unresolved_after.len(),
        1,
        "resolving tree_size=10 must not resolve tree_size=20 on the same network"
    );
    assert_eq!(unresolved_after[0].tree_size, 20);

    // The unrelated network's finding must be untouched by any of this.
    let other_unresolved = unresolved_equivocations(&pool, &other_network_id)
        .await
        .expect("unresolved_equivocations failed");
    assert_eq!(
        other_unresolved.len(),
        1,
        "resolving one network's finding must never resolve another network's"
    );

    // Re-resolving an already-resolved finding is a no-op, not an error.
    let resolved_again = resolve_equivocation(&pool, &network_id, 10, &"aa".repeat(32))
        .await
        .expect("re-resolving must not error");
    assert_eq!(resolved_again, 0);
}

/// The `resolve_equivocation` write itself is durable and records which
/// root_hash the operator determined was legitimate.
#[tokio::test]
#[ignore]
async fn resolving_records_the_legitimate_root_hash_and_timestamp() {
    let pool = test_pool().await;
    let network_id = unique_network_id("records-legitimate-hash");
    let legitimate = "ab".repeat(32);

    insert_finding(&pool, &network_id, 42, &legitimate, &"cd".repeat(32)).await;

    let resolved_count = resolve_equivocation(&pool, &network_id, 42, &legitimate)
        .await
        .expect("resolve_equivocation failed");
    assert_eq!(resolved_count, 1);

    let all = mirror::list_equivocations(&pool, &network_id)
        .await
        .expect("list_equivocations failed");
    let finding = all
        .iter()
        .find(|f| f.tree_size == 42)
        .expect("finding should still be listed after resolution");
    assert_eq!(
        finding.resolved_root_hash.as_deref(),
        Some(legitimate.as_str())
    );
    assert!(
        finding.resolved_at.is_some(),
        "resolved_at must be set once resolved"
    );
}

/// Recovery's data-loss invariant: `discard_mirrored_entries_from` removes
/// only entries verified at or after the conflicting `tree_size`, leaving
/// every entry verified strictly below it — the part of the branch that
/// was never in dispute — untouched.
#[tokio::test]
#[ignore]
async fn discard_only_removes_entries_at_or_after_the_conflicting_tree_size() {
    let pool = test_pool().await;
    let network_id = unique_network_id("discard-from-tree-size");

    // Entries verified against tree sizes 5, 10 (the conflict point), and
    // 15 — only the last two should be discarded when recovering from a
    // conflict first observed at tree_size=10.
    for (seq, verified_tree_size) in [(1i64, 5i64), (2, 10), (3, 15)] {
        mirror::insert_mirrored_entry(&pool, &mirrored_entry(&network_id, seq, verified_tree_size))
            .await
            .expect("insert_mirrored_entry failed");
    }

    let progress_before = mirror::mirrored_progress(&pool, &network_id, None)
        .await
        .expect("mirrored_progress failed");
    assert_eq!(progress_before.verified_count, 3);

    let discarded = discard_mirrored_entries_from(&pool, &network_id, 10)
        .await
        .expect("discard_mirrored_entries_from failed");
    assert_eq!(
        discarded, 2,
        "should discard the entries at tree_size 10 and 15"
    );

    let progress_after = mirror::mirrored_progress(&pool, &network_id, None)
        .await
        .expect("mirrored_progress failed");
    assert_eq!(
        progress_after.verified_count, 1,
        "the entry verified at tree_size=5, below the conflict, must survive"
    );
    assert_eq!(
        progress_after.last_seq, 1,
        "last_seq must reflect only the surviving entry"
    );
}

/// Discarding from a network with no mirrored entries at all is a clean
/// no-op, not an error — recovery on a network that never got far enough
/// to mirror anything past the conflict point.
#[tokio::test]
#[ignore]
async fn discard_on_a_network_with_no_mirrored_entries_is_a_harmless_no_op() {
    let pool = test_pool().await;
    let network_id = unique_network_id("discard-empty");

    let discarded = discard_mirrored_entries_from(&pool, &network_id, 1)
        .await
        .expect("discard_mirrored_entries_from failed");
    assert_eq!(discarded, 0);
}

/// End-to-end recovery lifecycle: detect (two disagreeing observations),
/// confirm the network is gated (unresolved), simulate the entries already
/// mirrored from both the legitimate and losing branches, resolve in favor
/// of the legitimate root hash, discard from the conflict point, and
/// confirm the mirror is left with exactly the legitimate branch's
/// below-conflict history intact and ready to resync the rest.
#[tokio::test]
#[ignore]
async fn full_recovery_lifecycle_leaves_only_pre_conflict_entries_and_clears_the_gate() {
    let pool = test_pool().await;
    let network_id = unique_network_id("full-lifecycle");
    let legitimate_root = "11".repeat(32);
    let bad_root = "22".repeat(32);
    let conflict_tree_size = 7i64;

    // Entries mirrored before the fork point — must survive recovery.
    mirror::insert_mirrored_entry(&pool, &mirrored_entry(&network_id, 1, 3))
        .await
        .expect("insert_mirrored_entry failed");
    mirror::insert_mirrored_entry(&pool, &mirrored_entry(&network_id, 2, 5))
        .await
        .expect("insert_mirrored_entry failed");
    // An entry mirrored from the (as it turns out) losing branch at the
    // conflict tree_size — must be discarded.
    mirror::insert_mirrored_entry(&pool, &mirrored_entry(&network_id, 3, conflict_tree_size))
        .await
        .expect("insert_mirrored_entry failed");

    // Detection: two sources disagree at the same network_id/tree_size.
    insert_finding(
        &pool,
        &network_id,
        conflict_tree_size,
        &legitimate_root,
        &bad_root,
    )
    .await;

    // Gate: the network is not eligible for further backfill while unresolved.
    let unresolved = unresolved_equivocations(&pool, &network_id)
        .await
        .expect("unresolved_equivocations failed");
    assert_eq!(
        unresolved.len(),
        1,
        "network should be gated after detection"
    );

    // Investigation concludes: legitimate_root was genuine.
    let resolved_count =
        resolve_equivocation(&pool, &network_id, conflict_tree_size, &legitimate_root)
            .await
            .expect("resolve_equivocation failed");
    assert_eq!(resolved_count, 1);

    // Gate clears.
    let unresolved_after = unresolved_equivocations(&pool, &network_id)
        .await
        .expect("unresolved_equivocations failed");
    assert!(
        unresolved_after.is_empty(),
        "network must no longer be gated once resolved"
    );

    // Recovery: discard everything at/after the conflict point (coarse by
    // design — see discard_mirrored_entries_from's own doc comment) so the
    // next backfill tick re-fetches and re-verifies from the now-trusted
    // branch rather than trusting what was already stored.
    let discarded = discard_mirrored_entries_from(&pool, &network_id, conflict_tree_size)
        .await
        .expect("discard_mirrored_entries_from failed");
    assert_eq!(discarded, 1);

    let progress = mirror::mirrored_progress(&pool, &network_id, None)
        .await
        .expect("mirrored_progress failed");
    assert_eq!(
        progress.verified_count, 2,
        "the two pre-conflict entries must survive recovery untouched"
    );
    assert_eq!(progress.last_seq, 2);
}
