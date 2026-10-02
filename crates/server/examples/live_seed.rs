//! Seeds and reads one harness schema for `scripts/nat-scenarios.sh`:
//! `live_seed session <schema>` inserts an identity with a session and prints its token;
//! `live_seed replicas <schema>` prints each replicated guild message as `<id> <replicated_by>`.

use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (action, schema) = (
        args.next().unwrap_or_default(),
        args.next().unwrap_or_default(),
    );
    if !schema.starts_with("live_")
        || !schema
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        anyhow::bail!("usage: live_seed session|replicas <live_ schema>");
    }
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL")?;
    let sep = if url.contains('?') { '&' } else { '?' };
    let pool = PgPoolOptions::new()
        .connect(&format!("{url}{sep}options=-c%20search_path%3D{schema}"))
        .await?;
    match action.as_str() {
        "session" => {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO identities (id) VALUES ($1)")
                .bind(id)
                .execute(&pool)
                .await?;
            sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
                .bind(id)
                .bind(format!("lab-{id}"))
                .execute(&pool)
                .await?;
            let token = format!("lab-token-{}", Uuid::new_v4());
            let expires = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
            sqlx::query(
                "INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)",
            )
            .bind(&token)
            .bind(id)
            .bind(expires)
            .execute(&pool)
            .await?;
            println!("{token}");
        }
        "replicas" => {
            for row in sqlx::query("SELECT id, replicated_by FROM guild_messages_replica")
                .fetch_all(&pool)
                .await?
            {
                let id: Uuid = row.try_get("id")?;
                let by: String = row.try_get("replicated_by")?;
                println!("{id} {by}");
            }
        }
        _ => anyhow::bail!("usage: live_seed session|replicas <live_ schema>"),
    }
    Ok(())
}
