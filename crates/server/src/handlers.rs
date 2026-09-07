use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{generate_session_token, hash_password, verify_password};
use crate::error::AppError;
use crate::state::AppState;

const SESSION_LIFETIME_DAYS: i64 = 30;

fn bearer_token(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)
}

/// Validates the bearer token against the `sessions` table and returns the
/// identity it belongs to. A missing, unknown, or expired token is always
/// `AppError::Unauthorized` — never distinguished in the response, so a
/// caller can't probe for which tokens once existed.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Uuid, AppError> {
    let token = bearer_token(headers)?;
    let row = sqlx::query!(
        "SELECT identity_id, expires_at FROM sessions WHERE token = $1",
        token
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::Unauthorized)?;

    if row.expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::Unauthorized);
    }
    Ok(row.identity_id)
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub identity_id: Uuid,
}

pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, AppError> {
    let identity_id = Uuid::new_v4();
    let password_hash = hash_password(&body.password);

    let mut tx = state.pool.begin().await?;

    sqlx::query!("INSERT INTO identities (id) VALUES ($1)", identity_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query!(
        "INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)",
        identity_id,
        body.username
    )
    .execute(&mut *tx)
    .await?;

    let insert_credential = sqlx::query!(
        "INSERT INTO credentials (identity_id, username, password_hash) VALUES ($1, $2, $3)",
        identity_id,
        body.username,
        password_hash
    )
    .execute(&mut *tx)
    .await;

    if let Err(sqlx::Error::Database(db_err)) = &insert_credential {
        if db_err.is_unique_violation() {
            return Err(AppError::UsernameTaken);
        }
    }
    insert_credential?;

    tx.commit().await?;

    Ok(Json(RegisterResponse { identity_id }))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    let row = sqlx::query!(
        "SELECT identity_id, password_hash FROM credentials WHERE username = $1",
        body.username
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::InvalidCredentials)?;

    if !verify_password(&body.password, &row.password_hash) {
        return Err(AppError::InvalidCredentials);
    }

    let token = generate_session_token();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::days(SESSION_LIFETIME_DAYS);

    sqlx::query!(
        "INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)",
        token,
        row.identity_id,
        expires_at
    )
    .execute(&state.pool)
    .await?;

    Ok(Json(LoginResponse { token, expires_at }))
}

#[derive(Serialize)]
pub struct ProfileResponse {
    pub identity_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub identity_created_at: OffsetDateTime,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query!(
        r#"
        SELECT p.display_name, p.avatar_url, i.created_at
        FROM profiles p
        JOIN identities i ON i.id = p.identity_id
        WHERE p.identity_id = $1
        "#,
        identity_id
    )
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(ProfileResponse {
        identity_id,
        identity_created_at: row.created_at,
        display_name: row.display_name,
        avatar_url: row.avatar_url,
    }))
}

#[derive(Deserialize)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

pub async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query!(
        r#"
        UPDATE profiles p
        SET display_name = COALESCE($2, p.display_name),
            avatar_url = COALESCE($3, p.avatar_url)
        FROM identities i
        WHERE p.identity_id = $1 AND i.id = p.identity_id
        RETURNING p.display_name, p.avatar_url, i.created_at
        "#,
        identity_id,
        body.display_name,
        body.avatar_url
    )
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(ProfileResponse {
        identity_id,
        identity_created_at: row.created_at,
        display_name: row.display_name,
        avatar_url: row.avatar_url,
    }))
}
