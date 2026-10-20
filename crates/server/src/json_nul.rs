//! Rejects U+0000 in any JSON request body before a handler runs (#1373): PostgreSQL TEXT and JSONB
//! cannot hold it, so a body carrying one would otherwise end in a database error and a 500.

use avalon_protocol::canonical_payload::CanonicalPayloadError;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::AppError;

/// True when `body` holds a raw NUL byte or a `\u0000` escape (an escaped backslash before `u0000` is not one).
pub(crate) fn contains_nul(body: &[u8]) -> bool {
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            0 => return true,
            b'\\' if body[i + 1..].starts_with(b"u0000") => return true,
            b'\\' => i += 1,
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_json(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            let essence = v
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            essence == "application/json" || essence.ends_with("+json")
        })
}

pub(crate) async fn reject_nul(request: Request, next: Next) -> Response {
    if !is_json(&request) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, crate::max_body_bytes_from_env()).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    if contains_nul(&bytes) {
        return AppError::InvalidPayload(CanonicalPayloadError::NulCharacter).into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/echo",
                post(|Json(v): Json<serde_json::Value>| async move { Json(v) }),
            )
            .layer(axum::middleware::from_fn(reject_nul))
    }

    async fn send(content_type: &str, body: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method("POST")
            .uri("/echo")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body.to_owned()))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[test]
    fn finds_escaped_and_raw_nul_but_not_an_escaped_backslash() {
        assert!(contains_nul(br#"{"a":"x\u0000y"}"#));
        assert!(contains_nul(br#"{"a\u0000":1}"#));
        assert!(contains_nul(b"{\"a\":\"x\0y\"}"));
        assert!(!contains_nul(br#"{"a":"x\\u0000y"}"#));
        assert!(contains_nul(br#"{"a":"\\\u0000"}"#));
        assert!(!contains_nul(br#"{"a":"\u0001 x \n"}"#));
    }

    #[tokio::test]
    async fn rejects_a_nul_in_any_json_string_with_a_typed_error() {
        for body in [
            r#"{"display_name":"a\u0000b"}"#,
            r#"{"nested":{"list":["ok","\u0000"]}}"#,
            r#"{"key\u0000":1}"#,
        ] {
            let (status, text) = send("application/json", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(text.contains("INVALID_PAYLOAD"), "{text}");
        }
        let (status, _) = send("application/json; charset=utf-8", r#"["\u0000"]"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn passes_clean_bodies_through_unchanged() {
        let (status, text) = send("application/json", r#"{"a":"x\\u0000y","b":"c"}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(text, r#"{"a":"x\\u0000y","b":"c"}"#);
    }

    #[tokio::test]
    async fn leaves_non_json_bodies_alone() {
        let (status, _) = send("text/plain", r#"["\u0000"]"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
