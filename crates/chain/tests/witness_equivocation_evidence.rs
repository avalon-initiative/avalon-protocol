//! Exercises `avalon_chain::mirror::record_witness_equivocation_evidence`/
//! `witness_equivocation_evidence_for` against a real Postgres instance —
//! the `equivocation_evidence` table migration
//! (`0076_equivocation_evidence`) plus the read/write pair built on top of
//! it. Gated `--ignored`, same convention as
//! `crates/chain/tests/witness_cosignatures.rs`.

use avalon_chain::mirror::{
    record_witness_equivocation_evidence, witness_equivocation_evidence_for,
    EquivocationEvidenceKind, WitnessEquivocationEvidence,
};
use avalon_protocol::cosigned_sth::{find_equivocating_witnesses, CosignedTreeHead};
use avalon_protocol::sth::sign_tree_head;
use avalon_protocol::witness::sign_witness_cosignature;
use ed25519_dalek::SigningKey;
use rand::RngExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn fresh_test_tree_size() -> i64 {
    rand::rng().random_range(1_000_000_000..2_000_000_000)
}

fn root_hash(byte: u8) -> String {
    hex::encode([byte; 32])
}

fn head(
    author_key: &SigningKey,
    tree_size: i64,
    root_hash: &str,
    network_id: &str,
    created_at: OffsetDateTime,
    cosigners: &[(&SigningKey, &str)],
) -> CosignedTreeHead {
    let sth = sign_tree_head(
        author_key,
        "settlement-operator-1",
        tree_size,
        root_hash,
        network_id,
        created_at,
    );
    let cosignatures = cosigners
        .iter()
        .map(|(key, id)| {
            sign_witness_cosignature(
                key, id, tree_size, root_hash, network_id, created_at, created_at,
            )
        })
        .collect();
    CosignedTreeHead { sth, cosignatures }
}

#[tokio::test]
#[ignore]
async fn stored_evidence_round_trips_and_stays_independently_verifiable() {
    let pool = test_pool().await;
    let network_id = format!("avalon-witness-equivocation-live-test-{}", Uuid::new_v4());
    let shard_id = "core";
    let tree_size = fresh_test_tree_size();
    let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000);

    let author_key = SigningKey::generate(&mut rand::rng());
    let author_verifying_key = author_key.verifying_key();
    let witness_a = SigningKey::generate(&mut rand::rng());
    let witness_b = SigningKey::generate(&mut rand::rng());
    let witness_c = SigningKey::generate(&mut rand::rng());
    let known_list = vec![
        ("witness-a".to_string(), witness_a.verifying_key()),
        ("witness-b".to_string(), witness_b.verifying_key()),
        ("witness-c".to_string(), witness_c.verifying_key()),
    ];

    // Two conflicting roots at the same tree_size, each independently
    // cosigned by a majority of the 3-witness list (threshold 2) —
    // witness_b double-signed both, witness_a and witness_c each signed
    // only one side.
    let head_a = head(
        &author_key,
        tree_size,
        &root_hash(1),
        &network_id,
        now,
        &[(&witness_a, "witness-a"), (&witness_b, "witness-b")],
    );
    let head_b = head(
        &author_key,
        tree_size,
        &root_hash(2),
        &network_id,
        now,
        &[(&witness_b, "witness-b"), (&witness_c, "witness-c")],
    );

    let freshness_cutoff = now - time::Duration::minutes(10);
    let equivocators = find_equivocating_witnesses(
        &author_verifying_key,
        &known_list,
        freshness_cutoff,
        now,
        &head_a,
        &head_b,
    );
    assert_eq!(equivocators, vec!["witness-b".to_string()]);

    let evidence = WitnessEquivocationEvidence {
        kind: EquivocationEvidenceKind::Witness,
        network_id: network_id.clone(),
        shard_id: shard_id.to_string(),
        tree_size,
        head_a: head_a.clone(),
        head_b: head_b.clone(),
        equivocating_witness_key_ids: equivocators.clone(),
        detected_at: None,
    };
    record_witness_equivocation_evidence(&pool, &evidence)
        .await
        .expect("recording witness equivocation evidence should succeed");

    // Idempotent replay of the same conflicting pair is a no-op, not a
    // duplicate row — a mirror re-polling the same two peers on a later
    // tick must not grow this table unboundedly for one ongoing conflict.
    record_witness_equivocation_evidence(&pool, &evidence)
        .await
        .expect("replaying the same evidence should not error");

    let stored = witness_equivocation_evidence_for(&pool, &network_id, shard_id)
        .await
        .expect("reading back evidence should succeed");
    assert_eq!(
        stored.len(),
        1,
        "the replay must not have duplicated the row"
    );
    let row = &stored[0];
    assert!(row.detected_at.is_some());
    assert_eq!(row.kind, EquivocationEvidenceKind::Witness);
    assert_eq!(row.tree_size, tree_size);
    assert_eq!(
        row.equivocating_witness_key_ids,
        vec!["witness-b".to_string()]
    );

    // The whole point: this evidence is independently re-verifiable from
    // signatures alone, using only what was read back from storage — no
    // trust in this node's own bookkeeping required.
    let (stored_a, stored_b) = if row.head_a.sth.root_hash == head_a.sth.root_hash {
        (&row.head_a, &row.head_b)
    } else {
        (&row.head_b, &row.head_a)
    };
    assert_eq!(stored_a.sth, head_a.sth);
    assert_eq!(stored_b.sth, head_b.sth);
    let reverified = find_equivocating_witnesses(
        &author_verifying_key,
        &known_list,
        freshness_cutoff,
        now,
        stored_a,
        stored_b,
    );
    assert_eq!(reverified, vec!["witness-b".to_string()]);
}

#[tokio::test]
#[ignore]
async fn evidence_is_recorded_the_same_regardless_of_which_head_is_passed_first() {
    let pool = test_pool().await;
    let network_id = format!("avalon-witness-equivocation-live-test-{}", Uuid::new_v4());
    let shard_id = "core";
    let tree_size = fresh_test_tree_size();
    let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000);

    let author_key = SigningKey::generate(&mut rand::rng());
    let witness_a = SigningKey::generate(&mut rand::rng());
    let witness_b = SigningKey::generate(&mut rand::rng());

    let head_a = head(
        &author_key,
        tree_size,
        &root_hash(3),
        &network_id,
        now,
        &[(&witness_a, "witness-a")],
    );
    let head_b = head(
        &author_key,
        tree_size,
        &root_hash(4),
        &network_id,
        now,
        &[(&witness_b, "witness-b")],
    );

    // Order A, B
    record_witness_equivocation_evidence(
        &pool,
        &WitnessEquivocationEvidence {
            kind: EquivocationEvidenceKind::Witness,
            network_id: network_id.clone(),
            shard_id: shard_id.to_string(),
            tree_size,
            head_a: head_a.clone(),
            head_b: head_b.clone(),
            equivocating_witness_key_ids: vec!["witness-a".to_string()],
            detected_at: None,
        },
    )
    .await
    .unwrap();

    // Same pair, order B, A — must land on the same row (ON CONFLICT DO
    // NOTHING against the lexically-normalized root hash ordering), not a
    // second row under swapped labels.
    record_witness_equivocation_evidence(
        &pool,
        &WitnessEquivocationEvidence {
            kind: EquivocationEvidenceKind::Witness,
            network_id: network_id.clone(),
            shard_id: shard_id.to_string(),
            tree_size,
            head_a: head_b,
            head_b: head_a,
            equivocating_witness_key_ids: vec!["witness-a".to_string()],
            detected_at: None,
        },
    )
    .await
    .unwrap();

    let stored = witness_equivocation_evidence_for(&pool, &network_id, shard_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
}

#[tokio::test]
#[ignore]
async fn author_level_evidence_round_trips_with_no_cosignatures_or_witnesses() {
    let pool = test_pool().await;
    let network_id = format!("avalon-witness-equivocation-live-test-{}", Uuid::new_v4());
    let shard_id = "game:author-fork";
    let tree_size = fresh_test_tree_size();
    let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000);
    let author_key = SigningKey::generate(&mut rand::rng());

    let head_a = head(&author_key, tree_size, &root_hash(5), &network_id, now, &[]);
    let head_b = head(&author_key, tree_size, &root_hash(6), &network_id, now, &[]);
    let evidence = WitnessEquivocationEvidence {
        kind: EquivocationEvidenceKind::Author,
        network_id: network_id.clone(),
        shard_id: shard_id.to_string(),
        tree_size,
        head_a: head_b.clone(),
        head_b: head_a.clone(),
        equivocating_witness_key_ids: Vec::new(),
        detected_at: None,
    };
    record_witness_equivocation_evidence(&pool, &evidence)
        .await
        .expect("author-level evidence with an empty witness list should be storable");
    record_witness_equivocation_evidence(&pool, &evidence)
        .await
        .unwrap();

    let stored = witness_equivocation_evidence_for(&pool, &network_id, shard_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    let row = &stored[0];
    assert_eq!(row.kind, EquivocationEvidenceKind::Author);
    assert!(row.equivocating_witness_key_ids.is_empty());
    assert!(row.head_a.cosignatures.is_empty() && row.head_b.cosignatures.is_empty());
    assert_eq!(row.head_a.sth, head_a.sth);
    assert_eq!(row.head_b.sth, head_b.sth);
    assert!(avalon_protocol::sth::verify_tree_head(
        &author_key.verifying_key(),
        &row.head_a.sth
    ));
    assert!(avalon_protocol::sth::verify_tree_head(
        &author_key.verifying_key(),
        &row.head_b.sth
    ));
}

#[tokio::test]
#[ignore]
async fn witness_level_evidence_must_name_a_witness() {
    let pool = test_pool().await;
    let network_id = format!("avalon-witness-equivocation-live-test-{}", Uuid::new_v4());
    let tree_size = fresh_test_tree_size();
    let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000);
    let author_key = SigningKey::generate(&mut rand::rng());

    let result = record_witness_equivocation_evidence(
        &pool,
        &WitnessEquivocationEvidence {
            kind: EquivocationEvidenceKind::Witness,
            network_id,
            shard_id: "core".to_string(),
            tree_size,
            head_a: head(&author_key, tree_size, &root_hash(7), "n", now, &[]),
            head_b: head(&author_key, tree_size, &root_hash(8), "n", now, &[]),
            equivocating_witness_key_ids: Vec::new(),
            detected_at: None,
        },
    )
    .await;
    assert!(result.is_err());
}
