//! Payloads stored in a Postgres `JSONB` column re-canonicalize to the same
//! bytes they were hashed over. Gated `--ignored`: it needs a reachable
//! Postgres in `DATABASE_URL` and only touches a session-local temp table, so
//! a throwaway database is enough.

use avalon_protocol::canonical_payload::{canonicalize, canonicalize_str, parse_strict};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

const VALID: &[&str] = &[
    r#"{"b":1,"a":2,"c":{"z":[],"y":{}}}"#,
    "{\"\u{e000}\":1,\"\u{10000}\":2,\"\u{ffff}\":3,\"a\":4}",
    r#"{"n":[0,-1,9007199254740992,-9007199254740992,0.5,0.000001,1e-7,1.5e-7,12345678901234.5,5e-324]}"#,
    r#"{"s":"a\b\t\n\f\r\"\\\u0001\u001f\u007f é😀","":""}"#,
    r#"{"k":null,"t":true,"f":false,"nested":[[[{"x":[]}]]]}"#,
    "[]",
    "{}",
];

#[tokio::test]
#[ignore]
async fn stored_payloads_recanonicalize_identically() {
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::query("CREATE TEMP TABLE canonical_payload_probe (id int, payload jsonb)")
        .execute(&pool)
        .await
        .unwrap();

    for (id, text) in VALID.iter().enumerate() {
        let value = parse_strict(text).expect("valid payload");
        let before = canonicalize(&value).unwrap();
        assert_eq!(before, canonicalize_str(text).unwrap());
        sqlx::query("INSERT INTO canonical_payload_probe VALUES ($1, $2)")
            .bind(id as i32)
            .bind(&value)
            .execute(&pool)
            .await
            .unwrap();
        let stored: Value =
            sqlx::query("SELECT payload FROM canonical_payload_probe WHERE id = $1")
                .bind(id as i32)
                .fetch_one(&pool)
                .await
                .unwrap()
                .get("payload");
        assert_eq!(canonicalize(&stored).unwrap(), before, "{text}");
    }
}

/// Documents why duplicate keys and number text must be rejected before
/// storage: JSONB keeps the last duplicate and normalizes some number text.
#[tokio::test]
#[ignore]
async fn jsonb_silently_collapses_duplicates_and_rewrites_some_numbers() {
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let probe = |text: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query("SELECT ($1::text)::jsonb::text AS j")
                .bind(text)
                .fetch_one(&pool)
                .await
                .unwrap()
                .get::<String, _>("j")
        }
    };
    assert_eq!(probe(r#"{"a":1,"a":2}"#).await, r#"{"a": 2}"#);
    assert_eq!(probe("[1e2]").await, "[100]");
    assert_eq!(probe("[1E-7]").await, "[0.0000001]");
    assert_eq!(probe("[1.50]").await, "[1.50]");
}
