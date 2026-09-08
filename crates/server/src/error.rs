use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("identity id already taken")]
    IdentityIdTaken,
    #[error("webauthn ceremony not found or already used")]
    CeremonyNotFound,
    #[error("webauthn ceremony expired")]
    CeremonyExpired,
    #[error("webauthn verification failed")]
    WebauthnFailed,
    #[error("event signature verification failed")]
    InvalidEventSignature,
    #[error("identity not found")]
    IdentityNotFound,
    #[error("cannot friend yourself")]
    SelfFriendRequest,
    #[error("already friends")]
    AlreadyFriends,
    #[error("a friend request is already pending")]
    FriendRequestExists,
    #[error("friend request not found")]
    FriendRequestNotFound,
    #[error("not friends")]
    NotFriends,
    #[error("invalid presence query")]
    InvalidPresenceQuery,
    #[error("no profile matches that handle")]
    HandleNotFound,
    #[error("could not generate a unique handle, try a different display name")]
    HandleGenerationFailed,
    #[error("avatar_url must be an http(s) URL of 2048 characters or fewer")]
    InvalidAvatarUrl,
    #[error("cannot block yourself")]
    SelfBlock,
    #[error("already blocked")]
    AlreadyBlocked,
    #[error("not blocked")]
    BlockNotFound,
    #[error("signing key not found")]
    SigningKeyNotFound,
    #[error("device grant not found or already resolved")]
    DeviceGrantNotFound,
    #[error("device grant has expired")]
    DeviceGrantExpired,
    #[error("approving signing key is unknown or has been revoked")]
    ApproverKeyInvalid,
    #[error("grant approval signature verification failed")]
    InvalidGrantSignature,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("ledger error")]
    Ledger(#[from] avalon_chain::SettlementError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::IdentityIdTaken => StatusCode::CONFLICT,
            AppError::CeremonyNotFound | AppError::CeremonyExpired => StatusCode::BAD_REQUEST,
            AppError::WebauthnFailed | AppError::InvalidEventSignature => StatusCode::UNAUTHORIZED,
            AppError::IdentityNotFound | AppError::FriendRequestNotFound => StatusCode::NOT_FOUND,
            AppError::SelfFriendRequest | AppError::InvalidPresenceQuery => StatusCode::BAD_REQUEST,
            AppError::AlreadyFriends | AppError::FriendRequestExists => StatusCode::CONFLICT,
            AppError::NotFriends => StatusCode::NOT_FOUND,
            AppError::HandleNotFound => StatusCode::NOT_FOUND,
            AppError::HandleGenerationFailed => StatusCode::CONFLICT,
            AppError::InvalidAvatarUrl => StatusCode::BAD_REQUEST,
            AppError::SelfBlock => StatusCode::BAD_REQUEST,
            AppError::AlreadyBlocked => StatusCode::CONFLICT,
            AppError::BlockNotFound => StatusCode::NOT_FOUND,
            AppError::SigningKeyNotFound | AppError::DeviceGrantNotFound => StatusCode::NOT_FOUND,
            AppError::DeviceGrantExpired => StatusCode::GONE,
            AppError::ApproverKeyInvalid | AppError::InvalidGrantSignature => {
                StatusCode::UNAUTHORIZED
            }
            AppError::Database(_) | AppError::Ledger(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Never leak internal error detail (e.g. SQL error text) to the client —
        // log it server-side once real observability exists; for now the
        // generic message is the safe default.
        let message = match &self {
            AppError::Database(_) | AppError::Ledger(_) => "internal server error".to_string(),
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
