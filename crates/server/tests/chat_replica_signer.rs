//! Replica ownership and the signer migration, on an isolated schema.
//! Run with `cargo test -p avalon-server --test chat_replica_signer -- --ignored`
//! against a throwaway database (`DATABASE_URL`).

use avalon_chain::mirror::{insert_observation, ObservedSth};
use avalon_server::chat_replication::{store_event, ReplicationEvent, StoreOutcome};
use avalon_server::guild_messages::MessageResponse as ChannelMessage;
use avalon_server::migrate::{self, MigrationSource};
use avalon_server::mirror_push::exceeds_observed;
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

struct Scratch {
    admin: PgPool,
    name: String,
    pool: PgPool,
}

impl Scratch {
    async fn new(tag: &str) -> Self {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let name = format!("avalon_replica_{tag}_{}", std::process::id());
        for sql in [
            format!("DROP SCHEMA IF EXISTS {name} CASCADE"),
            format!("CREATE SCHEMA {name}"),
        ] {
            sqlx::query(AssertSqlSafe(sql))
                .execute(&admin)
                .await
                .unwrap();
        }
        let sep = if url.contains('?') { "&" } else { "?" };
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&format!("{url}{sep}options=-c%20search_path%3D{name}"))
            .await
            .unwrap();
        migrate::migrate_up(&pool, MigrationSource::Embedded)
            .await
            .unwrap();
        Self { admin, name, pool }
    }

    async fn drop(self) {
        self.pool.close().await;
        let sql = format!("DROP SCHEMA IF EXISTS {} CASCADE", self.name);
        sqlx::query(AssertSqlSafe(sql))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

fn message(channel_id: Uuid, body: &str) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        channel_id,
        author: avalon_protocol::ids::IdentityId::random_for_tests(),
        body: body.to_string(),
        sent_at: OffsetDateTime::now_utc(),
    }
}

async fn replica_row(pool: &PgPool, id: Uuid) -> (String, String, bool) {
    let row = sqlx::query(
        "SELECT body, replicated_by, deleted_at IS NOT NULL AS deleted \
         FROM guild_messages_replica WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.get("body"),
        row.get("replicated_by"),
        row.get("deleted"),
    )
}

async fn has_signer_column(pool: &PgPool) -> bool {
    sqlx::query_scalar(
        "SELECT count(*) = 2 FROM information_schema.columns WHERE table_schema = current_schema() \
         AND column_name = 'replicated_by' AND is_nullable = 'NO' AND column_default IS NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn the_signer_column_is_required_on_a_fresh_database() {
    let s = Scratch::new("fresh").await;
    assert!(has_signer_column(&s.pool).await);
    let missing = sqlx::query(
        "INSERT INTO guild_messages_replica (id, channel_id, author, body, sent_at) \
         VALUES ($1, $2, $3, 'x', now())",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(avalon_protocol::ids::IdentityId::random_for_tests())
    .execute(&s.pool)
    .await;
    assert!(missing.is_err(), "a row without a signer was accepted");
    s.drop().await;
}

#[tokio::test]
#[ignore]
async fn the_migration_applies_to_populated_replicas_and_leaves_old_rows_undeletable() {
    let s = Scratch::new("populated").await;
    // The identity-id migration sits above the signer one; revert both, then replay the signer
    // migration's own SQL against the populated tables.
    for _ in 0..2 {
        migrate::migrate_down_one(&s.pool, MigrationSource::Embedded)
            .await
            .unwrap();
    }
    assert!(
        !has_signer_column(&s.pool).await,
        "the latest migration is not the signer one"
    );

    let (channel, conversation) = (Uuid::new_v4(), Uuid::new_v4());
    let old = message(channel, "old");
    sqlx::query(
        "INSERT INTO guild_messages_replica (id, channel_id, author, body, sent_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(old.id)
    .bind(channel)
    .bind(Uuid::new_v4())
    .bind(&old.body)
    .bind(old.sent_at)
    .execute(&s.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO conversation_messages_replica (id, conversation_id, author, body, sent_at) \
         VALUES ($1, $2, $2, 'old', now())",
    )
    .bind(Uuid::new_v4())
    .bind(conversation)
    .execute(&s.pool)
    .await
    .unwrap();

    sqlx::raw_sql(include_str!(
        "../db/migrations/0082_chat_replica_signer/up.sql"
    ))
    .execute(&s.pool)
    .await
    .unwrap();
    assert!(has_signer_column(&s.pool).await);
    let unsigned: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM guild_messages_replica WHERE replicated_by = '') \
         + (SELECT count(*) FROM conversation_messages_replica WHERE replicated_by = '')",
    )
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(unsigned, 2);

    let delete = ReplicationEvent::ChannelMessageDeleted {
        channel_id: channel,
        message_id: old.id,
    };
    let outcome = store_event(&s.pool, "12D3KooWAnyPeer", &delete)
        .await
        .unwrap();
    assert_eq!(outcome, StoreOutcome::NotOwned);
    assert!(!replica_row(&s.pool, old.id).await.2);
    s.drop().await;
}

#[tokio::test]
#[ignore]
async fn a_delete_applies_only_to_rows_the_same_signer_inserted() {
    let s = Scratch::new("owner").await;
    let channel = Uuid::new_v4();
    let m = message(channel, "mine");
    let insert = ReplicationEvent::ChannelMessage(m.clone());
    assert_eq!(
        store_event(&s.pool, "peer-a", &insert).await.unwrap(),
        StoreOutcome::Stored
    );
    assert_eq!(
        replica_row(&s.pool, m.id).await,
        ("mine".into(), "peer-a".into(), false)
    );

    // Another signer cannot overwrite the row by re-inserting its id.
    let mut forged = m.clone();
    forged.body = "forged".into();
    store_event(&s.pool, "peer-b", &ReplicationEvent::ChannelMessage(forged))
        .await
        .unwrap();
    assert_eq!(
        replica_row(&s.pool, m.id).await,
        ("mine".into(), "peer-a".into(), false)
    );

    let delete = |channel_id| ReplicationEvent::ChannelMessageDeleted {
        channel_id,
        message_id: m.id,
    };
    assert_eq!(
        store_event(&s.pool, "peer-b", &delete(channel))
            .await
            .unwrap(),
        StoreOutcome::NotOwned
    );
    assert!(!replica_row(&s.pool, m.id).await.2);
    // The right signer naming the wrong channel does not match either.
    assert_eq!(
        store_event(&s.pool, "peer-a", &delete(Uuid::new_v4()))
            .await
            .unwrap(),
        StoreOutcome::NotOwned
    );
    assert!(!replica_row(&s.pool, m.id).await.2);

    assert_eq!(
        store_event(&s.pool, "peer-a", &delete(channel))
            .await
            .unwrap(),
        StoreOutcome::Stored
    );
    assert!(replica_row(&s.pool, m.id).await.2);
    // A retried delete is still fine.
    assert_eq!(
        store_event(&s.pool, "peer-a", &delete(channel))
            .await
            .unwrap(),
        StoreOutcome::Stored
    );
    s.drop().await;
}

#[tokio::test]
#[ignore]
async fn a_conversation_replica_keeps_the_first_signer() {
    let s = Scratch::new("conv").await;
    let conversation_id = Uuid::new_v4();
    let m = avalon_server::conversations::MessageResponse {
        id: Uuid::new_v4(),
        conversation_id,
        author: avalon_protocol::ids::IdentityId::random_for_tests(),
        body: "dm".into(),
        sent_at: OffsetDateTime::now_utc(),
    };
    for signer in ["peer-a", "peer-b"] {
        store_event(
            &s.pool,
            signer,
            &ReplicationEvent::ConversationMessage(m.clone()),
        )
        .await
        .unwrap();
    }
    let by: String =
        sqlx::query_scalar("SELECT replicated_by FROM conversation_messages_replica WHERE id = $1")
            .bind(m.id)
            .fetch_one(&s.pool)
            .await
            .unwrap();
    assert_eq!(by, "peer-a");
    s.drop().await;
}

#[tokio::test]
#[ignore]
async fn a_notification_must_announce_more_than_the_source_has_already_shown() {
    let s = Scratch::new("notify").await;
    let obs = |url: &str, shard: &str, size: i64| ObservedSth {
        source_url: url.into(),
        network_id: "net".into(),
        shard_id: shard.into(),
        tree_size: size,
        root_hash: format!("{size:064x}"),
        signature: "sig".into(),
        signing_key_id: "key".into(),
        created_at: OffsetDateTime::now_utc(),
        observed_at: OffsetDateTime::now_utc(),
    };
    insert_observation(&s.pool, &obs("http://src.test", "core", 10))
        .await
        .unwrap();
    insert_observation(&s.pool, &obs("http://other.test", "core", 99))
        .await
        .unwrap();
    let src = vec![("core".to_string(), "http://src.test".to_string())];
    assert!(!exceeds_observed(&s.pool, &src, "net", 10).await.unwrap());
    assert!(!exceeds_observed(&s.pool, &src, "net", 3).await.unwrap());
    assert!(exceeds_observed(&s.pool, &src, "net", 11).await.unwrap());
    // Another source's observations do not count, nor another network's.
    assert!(!exceeds_observed(&s.pool, &src, "other-net", 0)
        .await
        .unwrap());
    let two = vec![
        ("core".to_string(), "http://src.test".to_string()),
        ("aux".to_string(), "http://src.test".to_string()),
    ];
    assert!(exceeds_observed(&s.pool, &two, "net", 5).await.unwrap());
    s.drop().await;
}

#[tokio::test]
#[ignore]
async fn a_guild_replica_keeps_the_first_signer() {
    let s = Scratch::new("guild").await;
    let channel = Uuid::new_v4();
    let first = message(channel, "first");
    store_event(
        &s.pool,
        "peer-a",
        &ReplicationEvent::ChannelMessage(first.clone()),
    )
    .await
    .unwrap();
    let mut other = message(channel, "squatter");
    other.id = first.id;
    store_event(&s.pool, "peer-b", &ReplicationEvent::ChannelMessage(other))
        .await
        .unwrap();
    assert_eq!(
        replica_row(&s.pool, first.id).await,
        ("first".into(), "peer-a".into(), false)
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM guild_messages_replica")
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    s.drop().await;
}
