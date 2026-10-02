//! Live schema checks for self-certifying identity ids, run with `--ignored` against a migrated
//! Postgres: no identity-typed column may still be a UUID, and the database itself refuses an
//! identity row whose id is not derived from its inception key.

use avalon_protocol::identity_id::TestIdentity;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// Identity-typed columns that carry no foreign key (projections and replicas).
const UNREFERENCED_IDENTITY_COLUMNS: &[(&str, &str)] = &[
    ("guild_messages_replica", "author"),
    ("conversation_messages_replica", "author"),
    ("indexer_game_bindings", "identity_id"),
    ("indexer_game_data_instances", "subject"),
    ("identity_chain_events", "identity_id"),
    ("identity_chain_state", "identity_id"),
];

#[tokio::test]
#[ignore]
async fn every_identity_column_is_text_with_a_shape_check() {
    let pool = test_pool().await;

    let referencing: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, a.attname::text \
         FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1] \
         WHERE c.contype = 'f' AND c.confrelid = 'identities'::regclass",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        referencing.len() >= 30,
        "expected the full set of identity foreign keys, found {}",
        referencing.len()
    );

    let mut columns = referencing;
    columns.push(("identities".to_string(), "id".to_string()));
    columns.extend(
        UNREFERENCED_IDENTITY_COLUMNS
            .iter()
            .map(|(table, column)| (table.to_string(), column.to_string())),
    );
    for (table, column) in &columns {
        let data_type: String = sqlx::query_scalar(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2",
        )
        .bind(table.trim_matches('"'))
        .bind(column)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("{table}.{column}: {e}"));
        assert_eq!(data_type, "text", "{table}.{column} must be TEXT");
    }

    // By name, so a future identity column added as a UUID without a foreign key is caught too.
    let stray: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND data_type = 'uuid' \
           AND column_name IN ('identity_id', 'author', 'owner', 'subject', 'created_by', \
                               'applicant', 'decided_by', 'blocker', 'blocked', \
                               'guardian_identity_id', 'cancelled_by', 'a', 'b', 'from', 'to')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        stray.is_empty(),
        "identity-typed UUID columns remain: {stray:?}"
    );
}

#[tokio::test]
#[ignore]
async fn the_database_refuses_an_identity_not_derived_from_its_key() {
    let pool = test_pool().await;
    let honest = TestIdentity::new();
    let other = TestIdentity::new();

    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(honest.id)
        .bind(honest.public_key().to_vec())
        .execute(&pool)
        .await
        .expect("a derived id must be accepted");

    let mismatched = sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(other.id)
        .bind(honest.public_key().to_vec())
        .execute(&pool)
        .await;
    assert!(mismatched.is_err(), "an id not derived from the key must be refused");

    let malformed = sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind("not-a-self-certifying-id")
        .bind(other.public_key().to_vec())
        .execute(&pool)
        .await;
    assert!(malformed.is_err(), "a malformed id must be refused");

    let uppercase = sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, 'x')")
        .bind(honest.id.to_string().to_uppercase())
        .execute(&pool)
        .await;
    assert!(uppercase.is_err(), "the shape check refuses non-lowercase ids");

    sqlx::query("DELETE FROM identities WHERE id = $1")
        .bind(honest.id)
        .execute(&pool)
        .await
        .unwrap();
}
