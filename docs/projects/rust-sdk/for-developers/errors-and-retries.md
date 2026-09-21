# Errors and Retries

Every fallible SDK call returns `Result<T, SdkError>` — a small, stable,
protocol-level taxonomy, never a raw `reqwest::Error`/HTTP status code you'd
have to know HTTP to interpret. The full design rationale and server-side
half live in [`../../backend-server/architecture/sdk.md`](../architecture/sdk.md)'s "Error
handling and retries" section (issue #47) — this page is the short version
for someone integrating against it.

## The `SdkError` variants

```rust
match session.achievements().await {
    Ok(history) => { /* ... */ }
    Err(SdkError::Unauthorized) => { /* the session token itself is stale — re-authenticate */ }
    Err(SdkError::CapabilityNotGranted(capability)) => { /* ask the player to grant `capability` */ }
    Err(SdkError::NotFound(_)) => { /* the resource doesn't exist */ }
    Err(SdkError::Conflict(_)) => { /* already exists / already in that state */ }
    Err(SdkError::Rejected { reason }) => { /* the server made a final decision not to apply this — don't retry unchanged */ }
    Err(SdkError::Unavailable { retried, detail }) => { /* a node hiccup — safe to surface as "try again later," not a gameplay error */ }
    Err(SdkError::Protocol(_)) => { /* an unexpected response shape — likely a version mismatch */ }
    Err(other) => { /* NotConversationParticipant, MissingIssuerCredentials, WebSocket, device-login errors — see each variant's own rustdoc */ }
}
```

Every non-2xx response is mapped by HTTP status (a small, stable, always-
present signal) into one of these buckets; the message text inside each
bucket is the server's own stable `code` field, not free-text prose that's
free to reword — safe to match on if you need finer-grained handling than
the six buckets give you, though the typed variant should cover most needs.

## Retries

Connection errors, timeouts, and 502/503/504 are retried automatically with
exponential backoff and full jitter — but **only for calls where a retry is
provably safe**: every read qualifies automatically; a write only retries
when it carries something that makes it safe (an `Idempotency-Key` header
the server honors, an existing per-endpoint dedup key, or an inherently
idempotent HTTP method). A write with none of those gets exactly one
attempt.

Today, `Session::issue_achievement` is the one write with real
`Idempotency-Key` coverage — issuing an achievement twice due to a retried
request would be the worst failure mode (a spurious duplicate attestation),
so it was the concrete case worth wiring end-to-end first. Broader coverage
across other writes (`dm`, guild/conversation sends,
`publish_schema_version`/`publish_instance`) is a documented follow-up, not
assumed done — those stay single-attempt.

Tune this via `AvalonConfig::retry` (`RetryConfig { max_retries, base_delay,
request_timeout }`):

```rust
use std::time::Duration;
use avalon_sdk::RetryConfig;

let retry = RetryConfig {
    max_retries: 5,
    base_delay: Duration::from_millis(100),
    request_timeout: Duration::from_secs(5),
};
```

`RetryConfig::default()` (3 retries, 200ms base, 10s per-request timeout) is
a reasonable starting point for most integrations.

## Offline queuing is a separate, opt-in layer

None of the above is "offline support" — a call that fails still fails,
synchronously, to the caller. For queuing intent locally and draining it
once connectivity returns, see `crate::sync_journal`/`crate::submission`
(issue #110/#111/#113) and
[`../../backend-server/architecture/synchronization.md`](../../backend-server/architecture/synchronization.md).
That layer has its own, coarser-grained retry/backoff policy
(`BackoffPolicy`) suited to a queue an integrator drains on its own
schedule, not the per-request retry described above.
