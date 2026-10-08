//! A revocation is projected only when it carries the issuer's signature for the network of the
//! stream that delivered it. Gated `--ignored`: needs a migrated Postgres (`DATABASE_URL`).

use avalon_indexer::identity_proof::EventOrigin;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::IndexError;
use avalon_protocol::achievements::{revocation_signing_bytes, AttestationSigner};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::{AttestationId, GlobalId, IdentityId};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const NET: &str = "avalon-dev-proof-a";
const OTHER_NET: &str = "avalon-dev-proof-b";

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

struct Issuer {
    id: Uuid,
    slug: String,
    key_id: Uuid,
    key: SigningKey,
}

impl Issuer {
    fn new() -> Self {
        let seed = [Uuid::new_v4().into_bytes(), Uuid::new_v4().into_bytes()].concat();
        Self {
            id: Uuid::new_v4(),
            slug: format!("proof-{}", Uuid::new_v4().simple()),
            key_id: Uuid::new_v4(),
            key: SigningKey::from_bytes(&seed.try_into().unwrap()),
        }
    }

    fn issuer_ref(&self) -> String {
        format!("game:{}", self.slug)
    }
}

fn event(kind: &str, at: OffsetDateTime, payload: serde_json::Value) -> ProtocolEvent {
    let gid = GlobalId::new("game", "proof", "self", "x");
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: gid.clone(),
        subject: gid,
        payload,
        timestamp: at,
        version: 1,
        identity_chain: None,
    }
}

fn registered(issuer: &Issuer, at: OffsetDateTime) -> ProtocolEvent {
    event(
        "game.registered",
        at,
        serde_json::json!({
            "game_id": issuer.id, "slug": issuer.slug, "name": "n", "developer": "d",
            "category": "game", "requested_capabilities": [],
            "initial_key": {
                "key_id": issuer.key_id, "algorithm": "ed25519",
                "public_key": BASE64.encode(issuer.key.verifying_key().to_bytes()),
            },
        }),
    )
}

fn issued(
    issuer: &Issuer,
    subject: IdentityId,
    attestation: Uuid,
    at: OffsetDateTime,
) -> ProtocolEvent {
    event(
        "achievement.issued",
        at,
        serde_json::json!({
            "id": attestation, "issuer": issuer.issuer_ref(),
            "subject": subject,
            "achievement": format!("{}:achievement:x", issuer.issuer_ref()),
            "issued_at_micros": 1_700_000_000_000_000_i64,
            "proof": {"key_id": issuer.key_id, "algorithm": "ed25519", "bytes": "x"},
        }),
    )
}

fn revoked(
    issuer: &Issuer,
    attestation: Uuid,
    signed_for: &str,
    at: OffsetDateTime,
) -> ProtocolEvent {
    let signer = AttestationSigner {
        network_id: signed_for,
        claim_kind: "achievement",
        issuer_ref: &issuer.issuer_ref(),
        signing_key_id: issuer.key_id,
    };
    let bytes = revocation_signing_bytes(&signer, AttestationId(attestation), "cheating", "r");
    event(
        "achievement.revoked",
        at,
        serde_json::json!({
            "id": Uuid::new_v4(), "attestation_id": attestation, "issuer": issuer.issuer_ref(),
            "reason_code": "cheating", "reason": "r",
            "proof": {
                "key_id": issuer.key_id, "algorithm": "ed25519",
                "bytes": BASE64.encode(issuer.key.sign(&bytes).to_bytes()),
            },
        }),
    )
}

async fn apply(
    pool: &PgPool,
    event: &ProtocolEvent,
    origin: &EventOrigin,
) -> Result<(), IndexError> {
    let mut tx = pool.begin().await.unwrap();
    PostgresIndexer::new(pool.clone())
        .apply_in_tx_from(&mut tx, event, origin)
        .await?;
    tx.commit().await.unwrap();
    Ok(())
}

async fn is_revoked(pool: &PgPool, attestation: Uuid) -> bool {
    sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM indexer_attestations WHERE id = $1")
        .bind(attestation)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// An issuer registered and one attestation issued on `origin`, a minute before now.
async fn seeded(pool: &PgPool, origin: &EventOrigin) -> (Issuer, Uuid) {
    let who = avalon_protocol::identity_id::TestIdentity::new();
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(who.id)
        .bind(who.public_key().to_vec())
        .execute(pool)
        .await
        .unwrap();
    let (issuer, attestation) = (Issuer::new(), Uuid::new_v4());
    let before = OffsetDateTime::now_utc() - time::Duration::minutes(1);
    apply(pool, &registered(&issuer, before), origin)
        .await
        .unwrap();
    apply(pool, &issued(&issuer, who.id, attestation, before), origin)
        .await
        .unwrap();
    (issuer, attestation)
}

#[tokio::test]
#[ignore]
async fn a_signed_revocation_is_projected_and_an_unsigned_one_is_refused() {
    let pool = test_pool().await;
    let origin = EventOrigin::mirrored(NET, "core");
    let (issuer, attestation) = seeded(&pool, &origin).await;
    let now = OffsetDateTime::now_utc();

    let mut unsigned = revoked(&issuer, attestation, NET, now);
    unsigned.payload.as_object_mut().unwrap().remove("proof");
    assert!(matches!(
        apply(&pool, &unsigned, &origin).await,
        Err(IndexError::Rejected(_))
    ));
    assert!(!is_revoked(&pool, attestation).await);

    apply(&pool, &revoked(&issuer, attestation, NET, now), &origin)
        .await
        .unwrap();
    assert!(is_revoked(&pool, attestation).await);
}

#[tokio::test]
#[ignore]
async fn a_revocation_signed_for_another_network_is_refused_on_this_one() {
    let pool = test_pool().await;
    let origin = EventOrigin::mirrored(NET, "core");
    let (issuer, attestation) = seeded(&pool, &origin).await;

    let replayed = revoked(&issuer, attestation, OTHER_NET, OffsetDateTime::now_utc());
    assert!(matches!(
        apply(&pool, &replayed, &origin).await,
        Err(IndexError::Rejected(_))
    ));
    assert!(!is_revoked(&pool, attestation).await);
}

#[tokio::test]
#[ignore]
async fn a_revocation_for_an_unregistered_issuer_waits() {
    let pool = test_pool().await;
    let origin = EventOrigin::mirrored(NET, "core");
    let issuer = Issuer::new();

    let event = revoked(&issuer, Uuid::new_v4(), NET, OffsetDateTime::now_utc());
    assert!(matches!(
        apply(&pool, &event, &origin).await,
        Err(IndexError::AwaitingKey(_))
    ));
}

#[tokio::test]
#[ignore]
async fn one_issuer_cannot_revoke_another_issuers_attestation() {
    let pool = test_pool().await;
    let origin = EventOrigin::mirrored(NET, "core");
    let (_owner, attestation) = seeded(&pool, &origin).await;
    let other = Issuer::new();
    apply(
        &pool,
        &registered(
            &other,
            OffsetDateTime::now_utc() - time::Duration::minutes(1),
        ),
        &origin,
    )
    .await
    .unwrap();

    let event = revoked(&other, attestation, NET, OffsetDateTime::now_utc());
    assert!(matches!(
        apply(&pool, &event, &origin).await,
        Err(IndexError::Rejected(_))
    ));
    assert!(!is_revoked(&pool, attestation).await);
}

#[tokio::test]
#[ignore]
async fn a_key_revoked_before_the_revocation_cannot_sign_it() {
    let pool = test_pool().await;
    let origin = EventOrigin::mirrored(NET, "core");
    let (issuer, attestation) = seeded(&pool, &origin).await;
    let revoked_at = OffsetDateTime::now_utc() - time::Duration::seconds(30);
    let key_revoked = event(
        "issuer.key_revoked",
        revoked_at,
        serde_json::json!({
            "game_id": issuer.id, "slug": issuer.slug, "key_id": issuer.key_id,
            "revoked_at": revoked_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
        }),
    );
    apply(&pool, &key_revoked, &origin).await.unwrap();

    let event = revoked(&issuer, attestation, NET, OffsetDateTime::now_utc());
    assert!(matches!(
        apply(&pool, &event, &origin).await,
        Err(IndexError::Rejected(_))
    ));
    assert!(!is_revoked(&pool, attestation).await);
}
