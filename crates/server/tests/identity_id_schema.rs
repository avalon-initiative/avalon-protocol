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
    ("indexer_integrator_bindings", "identity_id"),
    ("indexer_integrator_data_instances", "subject"),
    ("identity_chain_events", "identity_id"),
    ("identity_chain_state", "identity_id"),
];

/// UUID column names that never hold an identity id, wherever they appear.
const GENERIC_UUID_COLUMNS: &[&str] = &[
    "id",
    "event_id",
    "batch_id",
    "guild_id",
    "channel_id",
    "conversation_id",
    "integrator_id",
    "game_id",
    "signing_key_id",
    "passkey_id",
    "grant_id",
    "nonce",
    "request_id",
    "attestation_id",
    "binding_id",
    "proof_key_id",
    "key_id",
    "client_entry_id",
    "resource_id",
    "recognizer_id",
    "recognized_id",
    "main_guild",
    "approved_by_signing_key_id",
    "message_id",
    "ticket_id",
];

/// TEXT columns with identity-style names that hold something other than an identity id.
const NON_ID_TEXT_COLUMNS: &[(&str, &str)] = &[("ledger_entries", "subject")];

/// Table-specific non-identity UUID columns not covered by the generic names.
const NON_IDENTITY_UUID_COLUMNS: &[(&str, &str)] = &[];

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
    let checks: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, pg_get_constraintdef(c.oid) FROM pg_constraint c \
         WHERE c.contype = 'c'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for (table, column) in &columns {
        if table != "identities" {
            assert!(
                checks.iter().any(|(t, def)| t == table
                    && (def.contains(&format!("(({column} ~"))
                        || def.contains(&format!("((\"{column}\" ~")))
                    && def.contains("[0-9a-f]{64}")),
                "{table}.{column} has no shape CHECK"
            );
        }
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

    // Every remaining UUID column must be a known non-identity one, so an identity-style column
    // under any other name (actor, requested_by, approved_by, ...) fails here.
    let uuid_columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND data_type = 'uuid' ORDER BY 1, 2",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let unknown: Vec<String> = uuid_columns
        .iter()
        .filter(|(table, column)| {
            !NON_IDENTITY_UUID_COLUMNS.contains(&(table.as_str(), column.as_str()))
        })
        .filter(|(_, column)| !GENERIC_UUID_COLUMNS.contains(&column.as_str()))
        .map(|(table, column)| format!("{table}.{column}"))
        .collect();
    assert!(
        unknown.is_empty(),
        "unclassified UUID columns (identity ids are TEXT; add genuine non-identity ones to the allowlist): {unknown:?}"
    );

    // TEXT columns with identity-style names must be converted ones or known non-id text.
    let converted: std::collections::HashSet<(String, String)> = columns.iter().cloned().collect();
    let text_columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND data_type = 'text' \
           AND column_name IN ('identity_id', 'author', 'owner', 'subject', 'created_by', \
                               'applicant', 'decided_by', 'blocker', 'blocked', \
                               'guardian_identity_id', 'cancelled_by', 'from', 'to')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let unlisted: Vec<String> = text_columns
        .into_iter()
        .filter(|c| {
            !converted.contains(c) && !NON_ID_TEXT_COLUMNS.contains(&(c.0.as_str(), c.1.as_str()))
        })
        .map(|(t, c)| format!("{t}.{c}"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "identity-style TEXT columns without a shape check: {unlisted:?}"
    );

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

    let mismatched =
        sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
            .bind(other.id)
            .bind(honest.public_key().to_vec())
            .execute(&pool)
            .await;
    assert!(
        mismatched.is_err(),
        "an id not derived from the key must be refused"
    );

    let malformed =
        sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
            .bind("not-a-self-certifying-id")
            .bind(other.public_key().to_vec())
            .execute(&pool)
            .await;
    assert!(malformed.is_err(), "a malformed id must be refused");

    let uppercase =
        sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, 'x')")
            .bind(honest.id.to_string().to_uppercase())
            .execute(&pool)
            .await;
    assert!(
        uppercase.is_err(),
        "the shape check refuses non-lowercase ids"
    );

    sqlx::query("DELETE FROM identities WHERE id = $1")
        .bind(honest.id)
        .execute(&pool)
        .await
        .unwrap();
}
