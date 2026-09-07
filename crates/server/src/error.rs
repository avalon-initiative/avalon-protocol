use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("username already taken")]
    UsernameTaken,
    #[error("unauthorized")]
    Unauthorized,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("ledger error")]
    Ledger(#[from] avalon_chain::SettlementError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::InvalidCredentials | AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::UsernameTaken => StatusCode::CONFLICT,
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
