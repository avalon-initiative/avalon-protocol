//! Centralized request execution (issue #47): retry with exponential
//! backoff and full jitter on connection errors, timeouts, and 502/503/504
//! — the transient-failure class a node hiccup produces — plus a shared
//! non-success-response -> [`SdkError`] mapper every call site in this
//! crate uses instead of matching `reqwest::StatusCode` ad hoc.
//!
//! [`send`] deliberately does **not** turn every non-2xx response into an
//! `Err` itself — only the retryable 5xx class, and only once retries are
//! exhausted (as [`SdkError::Unavailable`]). Every other status is
//! returned as `Ok(Response)`, exactly like the raw `reqwest` call sites
//! this replaced, so a caller with its own special-case status handling
//! (`conversations.rs`'s 403 -> `NotConversationParticipant`, most
//! obviously) still gets to inspect `response.status()` itself before
//! falling back to [`map_error_response`] for the generic case. This is
//! the deliberate design split: transport-level retry lives here, in one
//! place; response *interpretation* stays with whichever module actually
//! knows what a given endpoint's statuses mean.
//!
//! **Retries only ever happen for a caller that opts in via
//! `idempotent: true`** — a plain write (no `Idempotency-Key` attached)
//! passes `false` and gets exactly one attempt, matching #47's "never
//! retry a non-idempotent write" invariant. Every read passes `true`,
//! matching "reads retry freely."

use std::time::Duration;

use rand::RngExt;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Deserialize;

use crate::SdkError;

/// Retry/timeout tuning for every `avalon-server` call this SDK makes.
/// `AvalonConfig::retry` — `Default` gives sensible values for a normal
/// integration; a game with tighter latency budgets (or wanting no
/// retries at all, via `max_retries: 0`) overrides it explicitly.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// How many additional attempts an idempotent request gets after its
    /// first, on a retryable failure. `0` disables retries entirely.
    pub max_retries: u32,
    /// The backoff base — attempt `n`'s ceiling is
    /// `base_delay * 2^(n-1)`, capped at 30s; the actual sleep is chosen
    /// uniformly at random between zero and that ceiling ("full jitter",
    /// avoiding every retrying client waking up in lockstep).
    pub base_delay: Duration,
    /// Per-request timeout, applied to every individual attempt.
    pub request_timeout: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(200),
            request_timeout: Duration::from_secs(10),
        }
    }
}

const MAX_BACKOFF: Duration = Duration::from_secs(30);

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_retryable_transport(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect()
}

async fn backoff_sleep(retry: &RetryConfig, attempt: u32) {
    let ceiling = retry
        .base_delay
        .saturating_mul(1u32 << attempt.saturating_sub(1).min(16))
        .min(MAX_BACKOFF);
    let jittered_ms = if ceiling.is_zero() {
        0
    } else {
        rand::rng().random_range(0..=ceiling.as_millis() as u64)
    };
    tokio::time::sleep(Duration::from_millis(jittered_ms)).await;
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    #[serde(default)]
    code: Option<String>,
}

/// Reads a response's `{ "error": ..., "code": ... }` body (`AppError`'s
/// shape, #47) for its human message, falling back to the bare status line
/// if the body isn't readable JSON in that shape — an unexpected body
/// should never itself become a panic or a swallowed error.
async fn error_message(response: Response) -> String {
    let status = response.status();
    match response.json::<ErrorBody>().await {
        Ok(ErrorBody {
            code: Some(code), ..
        }) => code,
        Ok(ErrorBody { error, .. }) => error,
        Err(_) => status.to_string(),
    }
}

/// Sends one logical request, retrying on the transient-failure class
/// described in this module's doc comment when `idempotent` is `true`.
/// `build` is called fresh for every attempt (a `RequestBuilder` can't be
/// reused) — pass a closure, not an already-built builder.
pub(crate) async fn send<F>(
    client: &reqwest::Client,
    retry: &RetryConfig,
    idempotent: bool,
    mut build: F,
) -> Result<Response, SdkError>
where
    F: FnMut(&reqwest::Client) -> RequestBuilder,
{
    let max_retries = if idempotent { retry.max_retries } else { 0 };
    let mut attempt = 0u32;
    loop {
        let request = build(client).timeout(retry.request_timeout);
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if is_retryable_status(status) {
                    if attempt < max_retries {
                        attempt += 1;
                        backoff_sleep(retry, attempt).await;
                        continue;
                    }
                    let detail = error_message(response).await;
                    return Err(SdkError::Unavailable {
                        retried: attempt,
                        detail,
                    });
                }
                return Ok(response);
            }
            Err(err) => {
                if is_retryable_transport(&err) && attempt < max_retries {
                    attempt += 1;
                    backoff_sleep(retry, attempt).await;
                    continue;
                }
                if err.is_timeout() || err.is_connect() {
                    return Err(SdkError::Unavailable {
                        retried: attempt,
                        detail: err.to_string(),
                    });
                }
                return Err(SdkError::Protocol(err.to_string()));
            }
        }
    }
}

/// Retries a *whole* async operation — not a single HTTP request — up to
/// `retry.max_retries` times whenever it fails with
/// [`SdkError::Unavailable`] (the same retryable-failure class [`send`]
/// itself classifies). For a write whose safe-retry unit spans more than
/// one HTTP request (achievements issuance: challenge, then issue — the
/// challenge is single-use, so retrying only the second request with a
/// possibly-already-consumed challenge id would just fail differently),
/// `op` should redo the *whole* exchange from scratch on each attempt,
/// never resume partway — and every [`send`] call inside `op` should pass
/// `idempotent: false`, since retrying happens at this outer level
/// instead. Only ever call this around a write that itself carries an
/// idempotency key the server honors (issue #47's invariant) — `op`'s own
/// doc comment at each call site says which.
pub(crate) async fn retry_write<F, Fut, T>(retry: &RetryConfig, mut op: F) -> Result<T, SdkError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, SdkError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(SdkError::Unavailable { detail, .. }) if attempt < retry.max_retries => {
                attempt += 1;
                backoff_sleep(retry, attempt).await;
                let _ = detail;
            }
            Err(SdkError::Unavailable { detail, .. }) => {
                return Err(SdkError::Unavailable {
                    retried: attempt,
                    detail,
                });
            }
            Err(other) => return Err(other),
        }
    }
}

/// The default non-success -> [`SdkError`] mapping, for any call site with
/// no status code that means something special on that particular
/// endpoint. The *bucket* (which of the six typed variants) is chosen by
/// status — a small, stable, always-present signal every one of
/// `AppError`'s ~150 variants already funnels through — but the message
/// text carried inside that variant is the server's stable `code`
/// (`AppError::code`, #47), never the free-text `error` string, whenever
/// the response actually has one. This is #47's own invariant in
/// practice: a caller matching only on the `SdkError` variant gets a
/// stable six-way taxonomy; a caller that also inspects the message text
/// (e.g. `NotFound`'s payload) is reading `AppError::code`'s stable
/// identifier, not prose that's free to reword.
pub(crate) async fn map_error_response(response: Response) -> SdkError {
    let status = response.status();
    let body: Option<ErrorBody> = response.json().await.ok();
    let message = match &body {
        Some(ErrorBody {
            code: Some(code), ..
        }) => code.clone(),
        Some(ErrorBody { error, .. }) => error.clone(),
        None => status.to_string(),
    };
    match status {
        StatusCode::UNAUTHORIZED => SdkError::Unauthorized,
        StatusCode::FORBIDDEN => SdkError::CapabilityNotGranted(message),
        StatusCode::NOT_FOUND => SdkError::NotFound(message),
        StatusCode::CONFLICT => SdkError::Conflict(message),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            SdkError::Rejected { reason: message }
        }
        // Rate-limited or any server-side failure, whether or not it's one
        // of the narrower 502/503/504 class `send` itself auto-retries
        // (see this module's own doc comment) — the request was fine, the
        // server just couldn't answer it right now. `retried: 0` here
        // specifically means "not retried inside this one call," not
        // "this can't be retried" — a caller layering its own retry on
        // top (`crate::submission`'s journal, most notably) still should.
        StatusCode::TOO_MANY_REQUESTS => SdkError::Unavailable {
            retried: 0,
            detail: message,
        },
        _ if status.is_server_error() => SdkError::Unavailable {
            retried: 0,
            detail: message,
        },
        _ => SdkError::Protocol(format!("{status}: {message}")),
    }
}

/// `http(s)://` -> `ws(s)://` for a websocket endpoint on the same server —
/// shared by every `subscribe_*` method (`social::subscribe_presence`,
/// `guilds::ChannelHandle::subscribe_messages`,
/// `conversations::ConversationHandle::subscribe_messages`, issue #438).
pub(crate) fn websocket_url(server_url: &str, path: &str) -> String {
    if let Some(rest) = server_url.strip_prefix("https://") {
        format!("wss://{rest}{path}")
    } else if let Some(rest) = server_url.strip_prefix("http://") {
        format!("ws://{rest}{path}")
    } else {
        format!("{server_url}{path}")
    }
}

#[cfg(test)]
mod tests {
    //! A real local server (`wiremock`), not hand-built `reqwest::Response`
    //! values — this module's own logic (#47) is inseparable from actual
    //! `reqwest` behavior (timeouts, connection errors, header delivery),
    //! so these are worth the dependency even though nothing else in this
    //! crate's unit tests needs a live HTTP round trip. No `avalon-server`,
    //! no `--ignored`, no `make start` — these run under plain `cargo test`.

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;

    fn fast_retry(max_retries: u32) -> RetryConfig {
        RetryConfig {
            max_retries,
            base_delay: Duration::from_millis(1),
            request_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn maps_401_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"error": "unauthorized", "code": "UNAUTHORIZED"}),
            ))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let response = send(&client, &fast_retry(0), true, |c| {
            c.get(format!("{}/x", server.uri()))
        })
        .await
        .unwrap();
        assert!(matches!(
            map_error_response(response).await,
            SdkError::Unauthorized
        ));
    }

    #[tokio::test]
    async fn maps_403_capability_to_capability_not_granted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"error": "forbidden", "code": "FORBIDDEN"})),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let response = send(&client, &fast_retry(0), true, |c| {
            c.get(format!("{}/x", server.uri()))
        })
        .await
        .unwrap();
        assert!(matches!(
            map_error_response(response).await,
            SdkError::CapabilityNotGranted(_)
        ));
    }

    #[tokio::test]
    async fn maps_409_to_conflict() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(409).set_body_json(
                    serde_json::json!({"error": "conflict", "code": "ALREADY_FRIENDS"}),
                ),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let response = send(&client, &fast_retry(0), true, |c| {
            c.get(format!("{}/x", server.uri()))
        })
        .await
        .unwrap();
        assert!(matches!(
            map_error_response(response).await,
            SdkError::Conflict(_)
        ));
    }

    /// Fails the first `fail_times` requests with 503, then succeeds —
    /// simulating a real node hiccup that clears up, which `retries_on_503_then_succeeds`
    /// and `retries_write_with_idempotency_key_once_only` both need a real
    /// stateful server for (two independently-mounted `Mock`s racing on
    /// which one "wins" a given request isn't a reliable way to model
    /// this).
    struct FlakyThenOk {
        remaining_failures: AtomicU32,
    }

    impl Respond for FlakyThenOk {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let previous =
                self.remaining_failures
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                        if n > 0 {
                            Some(n - 1)
                        } else {
                            None
                        }
                    });
            match previous {
                Ok(_) => ResponseTemplate::new(503),
                Err(_) => ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})),
            }
        }
    }

    #[tokio::test]
    async fn retries_on_503_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(FlakyThenOk {
                remaining_failures: AtomicU32::new(1),
            })
            .expect(2)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let response = send(&client, &fast_retry(3), true, |c| {
            c.get(format!("{}/x", server.uri()))
        })
        .await
        .expect("a retried request should eventually succeed");
        assert!(response.status().is_success());
    }

    #[tokio::test]
    async fn does_not_retry_write_without_idempotency_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x"))
            .respond_with(FlakyThenOk {
                remaining_failures: AtomicU32::new(1),
            })
            // Exactly one request: a non-idempotent write must never be
            // retried, even though (per `FlakyThenOk`) a second attempt
            // would have succeeded.
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = send(&client, &fast_retry(3), false, |c| {
            c.post(format!("{}/x", server.uri()))
        })
        .await;
        assert!(matches!(
            result,
            Err(SdkError::Unavailable { retried: 0, .. })
        ));
    }

    #[tokio::test]
    async fn retries_write_with_idempotency_key_once_only() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x"))
            .respond_with(FlakyThenOk {
                remaining_failures: AtomicU32::new(1),
            })
            .expect(2)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let key = "fixed-idempotency-key";
        let response = send(&client, &fast_retry(3), true, |c| {
            c.post(format!("{}/x", server.uri()))
                .header("idempotency-key", key)
        })
        .await
        .expect("a retried, keyed write should eventually succeed");
        assert!(response.status().is_success());

        // The same key was sent on every attempt — never regenerated
        // per-attempt, which would defeat the point (see
        // `achievements.rs::submit_achievement_issuance`'s own doc
        // comment).
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(request.headers.get("idempotency-key").unwrap(), key);
        }
    }

    #[tokio::test]
    async fn timeout_yields_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .mount(&server)
            .await;

        let retry = RetryConfig {
            max_retries: 0,
            base_delay: Duration::from_millis(1),
            request_timeout: Duration::from_millis(20),
        };
        let client = reqwest::Client::new();
        let result = send(&client, &retry, true, |c| {
            c.get(format!("{}/x", server.uri()))
        })
        .await;
        assert!(matches!(
            result,
            Err(SdkError::Unavailable { retried: 0, .. })
        ));
    }

    #[test]
    fn websocket_url_swaps_http_scheme_for_ws() {
        assert_eq!(
            websocket_url("http://127.0.0.1:8080", "/ws/presence?token=abc"),
            "ws://127.0.0.1:8080/ws/presence?token=abc"
        );
    }

    #[test]
    fn websocket_url_swaps_https_scheme_for_wss() {
        assert_eq!(
            websocket_url("https://avalon.example", "/ws/presence?token=abc"),
            "wss://avalon.example/ws/presence?token=abc"
        );
    }
}
