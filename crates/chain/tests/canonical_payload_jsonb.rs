//! Payloads stored in a Postgres `JSONB` column re-canonicalize to the same
//! bytes they were hashed over. Gated `--ignored`: it needs a reachable
//! Postgres in `DATABASE_URL` and only touches a session-local temp table, so
//! a throwaway database is enough.

use avalon_protocol::canonical_payload::{canonicalize, canonicalize_str, parse_strict};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

/// Every vector the rule accepts, as `(name, jsonUtf8, canonicalUtf8)`.
fn accepted_vectors() -> Vec<(String, String, String)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/vectors/canonical-payload.json"
    );
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| {
            let canonical = v["expected"]["canonicalUtf8"].as_str()?;
            Some((
                v["name"].as_str().unwrap().to_string(),
                v["input"]["jsonUtf8"].as_str().unwrap().to_string(),
                canonical.to_string(),
            ))
        })
        .collect()
}

async fn probe_pool() -> sqlx::PgPool {
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect")
}

#[tokio::test]
#[ignore]
async fn stored_payloads_recanonicalize_identically() {
    let pool = probe_pool().await;
    sqlx::query("CREATE TEMP TABLE canonical_payload_probe (id int, payload jsonb)")
        .execute(&pool)
        .await
        .unwrap();

    let vectors = accepted_vectors();
    assert!(vectors.len() > 30, "too few accepted vectors");
    for (id, (name, text, canonical)) in vectors.iter().enumerate() {
        let value = parse_strict(text).expect("valid payload");
        let before = canonicalize(&value).unwrap();
        assert_eq!(&before, canonical, "{name}");
        assert_eq!(before, canonicalize_str(text).unwrap());
        sqlx::query("INSERT INTO canonical_payload_probe VALUES ($1, $2)")
            .bind(id as i32)
            .bind(&value)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let stored: Value =
            sqlx::query("SELECT payload FROM canonical_payload_probe WHERE id = $1")
                .bind(id as i32)
                .fetch_one(&pool)
                .await
                .unwrap()
                .get("payload");
        assert_eq!(&canonicalize(&stored).unwrap(), canonical, "{name}");
    }
}

/// Every Unicode scalar value the rule permits, as a value and as a key, survives JSONB.
#[tokio::test]
#[ignore]
async fn every_permitted_character_survives_jsonb() {
    let pool = probe_pool().await;
    let all: String = (1..=0x10FFFFu32).filter_map(char::from_u32).collect();
    let value = serde_json::json!({ all.clone(): [all.clone()] });
    let before = canonicalize(&value).unwrap();
    let stored: Value = sqlx::query("SELECT $1::jsonb AS j")
        .bind(&value)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("j");
    assert_eq!(canonicalize(&stored).unwrap(), before);
}

/// Why U+0000 is rejected by the rule: JSONB refuses it in a value and in a key.
#[tokio::test]
#[ignore]
async fn jsonb_refuses_nul_in_values_and_keys() {
    let pool = probe_pool().await;
    for value in [
        serde_json::json!({"a": "x\u{0}y"}),
        serde_json::json!({"x\u{0}y": 1}),
    ] {
        let err = sqlx::query("SELECT $1::jsonb AS j")
            .bind(&value)
            .fetch_one(&pool)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported Unicode escape sequence"),
            "{err}"
        );
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
