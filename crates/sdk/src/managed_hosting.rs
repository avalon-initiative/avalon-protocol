//! Issue #564: a client for #531's managed-hosting two-phase remote-signing
//! flow (`POST /ledger/prepare-batch` / `POST /ledger/finalize-batch`,
//! `crates/server/src/settlement.rs`) that can submit against more than one
//! candidate host, so an integrator isn't stuck waiting to *notice* a
//! stalled host before manually switching (#544's still-real gap, named
//! there as "a real option, not designed here").
//!
//! **The safe shape of "concurrent submission": prepare-race, finalize
//! once.** `avalon_chain::PostgresSettlementProvider::prepare` is read-only — it never
//! touches `ledger_entries`/`ledger_batches`, just previews the tree head
//! this batch would produce against that node's *current* tip
//! (`crates/chain/src/postgres.rs`'s own doc comment). That means fanning
//! `prepare-batch` out to every candidate host concurrently and taking
//! whichever answers first is free of the fork risk a concurrent *finalize*
//! fan-out would have: two independent managed-hosting nodes each hold
//! their own separate ledger for this shard, so committing the same batch
//! to two of them independently would append it after two different tips,
//! producing two genuinely different `entry_hash` chains for the same
//! logical history — not a race with one winner, an actual fork. This
//! client only ever calls `finalize-batch` against the *one* host whose
//! `prepare-batch` it used to build the signature, never more than one host
//! per batch. If that finalize itself fails (the chosen host went down
//! between prepare and finalize), it falls back to prepare-racing the
//! remaining candidates rather than retrying finalize blindly against a
//! host that may now be gone.
//!
//! Server-side, `avalon_chain::PostgresSettlementProvider::finalize`
//! is idempotent on a replayed `batch_id` (issue #564) — a retried finalize
//! against the *same* host (a dropped response, this client retrying after
//! a timeout without knowing whether the first attempt landed) returns the
//! existing commitment rather than erroring. That only de-duplicates
//! retries against one host; it does nothing for two *different* hosts,
//! which is exactly why this client's own single-finalize discipline is
//! what actually prevents a fork, not anything server-side.

use std::time::Duration;

use avalon_chain::sth::{self, PreparedTreeHead};
use avalon_protocol::events::{Commitment, EventBatch};
use ed25519_dalek::SigningKey;
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use serde::Serialize;

/// One managed host this client is willing to submit to. `submit_key` is
/// that host's own `AVALON_SETTLEMENT_SUBMIT_KEY` bearer value — see
/// `crates/server/src/settlement.rs::require_settlement_submit_key`; each
/// host can (and in a real multi-operator deployment, will) have a
/// different key.
#[derive(Debug, Clone)]
pub struct HostCandidate {
    /// This host's base URL, no trailing slash, e.g. `https://host-a.example`.
    pub url: String,
    /// This host's own `AVALON_SETTLEMENT_SUBMIT_KEY` bearer value.
    pub submit_key: String,
}

/// What can go wrong submitting through [`ManagedHostingClient::submit`].
#[derive(Debug, thiserror::Error)]
pub enum ManagedHostingError {
    /// [`ManagedHostingClient::new`] was given an empty host list — a
    /// clear, immediate error rather than a submit call that can never
    /// succeed.
    #[error("no host candidates configured")]
    NoCandidates,
    /// Every candidate's `prepare-batch` failed — carries each host's own
    /// error text, in the order candidates were given, for diagnostics.
    #[error("prepare-batch failed against every candidate host: {0:?}")]
    AllPrepareFailed(Vec<String>),
    /// The chosen host's `finalize-batch` failed and no other candidate was
    /// left to fall back to.
    #[error("finalize-batch failed against every candidate host: {0:?}")]
    AllFinalizeFailed(Vec<String>),
    /// A malformed/unexpected response, or a transport-level failure
    /// (connection error, timeout) talking to a candidate host.
    #[error("unexpected response from a managed-hosting endpoint: {0}")]
    Protocol(String),
}

fn base_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// A client for #531's managed-hosting two-phase flow with #564's
/// prepare-race/finalize-once failover across `hosts`. Owns no session —
/// this is node-to-node/integrator-to-managed-host traffic authenticated by
/// each host's own shared-secret bearer token, not a logged-in user's
/// session.
pub struct ManagedHostingClient {
    client: reqwest::Client,
    hosts: Vec<HostCandidate>,
}

impl ManagedHostingClient {
    /// `hosts` should be every managed host this integrator is willing to
    /// fail over across for one shard — typically 2, occasionally more.
    /// Order doesn't matter; every candidate is raced identically.
    pub fn new(hosts: Vec<HostCandidate>) -> Self {
        Self {
            client: reqwest::Client::new(),
            hosts,
        }
    }

    /// The full #564 flow: prepare-race across every not-yet-tried
    /// candidate, sign the winning preview locally with `signing_key`
    /// (this client's own key — no managed host ever sees it, per #531),
    /// then finalize against that same host. Falls back to prepare-racing
    /// the remaining candidates if finalize fails, up to once per
    /// candidate host total.
    pub async fn submit(
        &self,
        batch: &EventBatch,
        signing_key: &SigningKey,
        signing_key_id: &str,
    ) -> Result<Commitment, ManagedHostingError> {
        if self.hosts.is_empty() {
            return Err(ManagedHostingError::NoCandidates);
        }

        let mut tried = vec![false; self.hosts.len()];
        let mut finalize_errors = Vec::new();

        loop {
            let (index, preview) = self.race_prepare(batch, &tried).await?;
            tried[index] = true;
            let host = &self.hosts[index];

            let created_at = preview.created_at;
            let message = sth::signing_message(
                preview.tree_size,
                &preview.root_hash,
                &preview.network_id,
                created_at,
            );
            let signature =
                hex::encode(ed25519_dalek::Signer::sign(signing_key, &message).to_bytes());

            match self
                .finalize(host, batch, created_at, signing_key_id, &signature)
                .await
            {
                Ok(commitment) => return Ok(commitment),
                Err(e) => {
                    finalize_errors.push(format!("{}: {e}", host.url));
                    if tried.iter().all(|t| *t) {
                        return Err(ManagedHostingError::AllFinalizeFailed(finalize_errors));
                    }
                    // Otherwise loop: prepare-race the remaining
                    // (not-yet-tried) candidates and try again — this
                    // host's `prepare-batch` is stateless, so nothing it
                    // did needs to be undone.
                }
            }
        }
    }

    /// Fans `prepare-batch` out concurrently to every candidate not yet in
    /// `tried`, returning the first success along with that candidate's
    /// index into `self.hosts`. Safe to call more than once across
    /// retries — `prepare` never mutates state (see module docs).
    async fn race_prepare(
        &self,
        batch: &EventBatch,
        tried: &[bool],
    ) -> Result<(usize, PreparedTreeHead), ManagedHostingError> {
        let mut futures = FuturesUnordered::new();
        for (index, host) in self.hosts.iter().enumerate() {
            if tried[index] {
                continue;
            }
            futures.push(async move {
                self.prepare(host, batch)
                    .await
                    .map(|preview| (index, preview))
                    .map_err(|e| format!("{}: {e}", host.url))
            });
        }

        let mut errors = Vec::new();
        while let Some(result) = futures.next().await {
            match result {
                Ok(win) => return Ok(win),
                Err(e) => errors.push(e),
            }
        }
        Err(ManagedHostingError::AllPrepareFailed(errors))
    }

    async fn prepare(
        &self,
        host: &HostCandidate,
        batch: &EventBatch,
    ) -> Result<PreparedTreeHead, ManagedHostingError> {
        let response = self
            .client
            .post(format!("{}/ledger/prepare-batch", base_url(&host.url)))
            .bearer_auth(&host.submit_key)
            .json(batch)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ManagedHostingError::Protocol(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ManagedHostingError::Protocol(format!(
                "prepare-batch: {status}: {text}"
            )));
        }

        response
            .json::<PreparedTreeHead>()
            .await
            .map_err(|e| ManagedHostingError::Protocol(e.to_string()))
    }

    async fn finalize(
        &self,
        host: &HostCandidate,
        batch: &EventBatch,
        created_at: time::OffsetDateTime,
        signing_key_id: &str,
        signature: &str,
    ) -> Result<Commitment, ManagedHostingError> {
        #[derive(Serialize)]
        struct FinalizeBody<'a> {
            batch: &'a EventBatch,
            #[serde(with = "time::serde::rfc3339")]
            created_at: time::OffsetDateTime,
            signing_key_id: &'a str,
            signature: &'a str,
        }

        let response = self
            .client
            .post(format!("{}/ledger/finalize-batch", base_url(&host.url)))
            .bearer_auth(&host.submit_key)
            .json(&FinalizeBody {
                batch,
                created_at,
                signing_key_id,
                signature,
            })
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ManagedHostingError::Protocol(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ManagedHostingError::Protocol(format!(
                "finalize-batch: {status}: {text}"
            )));
        }

        response
            .json::<Commitment>()
            .await
            .map_err(|e| ManagedHostingError::Protocol(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    use ed25519_dalek::SigningKey;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_batch() -> EventBatch {
        EventBatch {
            id: Uuid::new_v4(),
            events: vec![],
            created_at: time::OffsetDateTime::now_utc(),
        }
    }

    fn preview_body(tree_size: i64) -> serde_json::Value {
        serde_json::to_value(PreparedTreeHead {
            batch_id: Uuid::new_v4(),
            tree_size,
            root_hash: "aa".repeat(32),
            network_id: "test-network".to_string(),
            created_at: time::OffsetDateTime::now_utc(),
        })
        .unwrap()
    }

    fn commitment_body() -> serde_json::Value {
        serde_json::to_value(Commitment {
            batch_id: Uuid::new_v4(),
            proof: vec![1, 2, 3],
            committed_at: time::OffsetDateTime::now_utc(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn submits_via_whichever_host_answers_prepare_first_and_never_finalizes_the_other() {
        let fast = MockServer::start().await;
        let slow = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ledger/prepare-batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(preview_body(5)))
            .mount(&fast)
            .await;
        Mock::given(method("POST"))
            .and(path("/ledger/finalize-batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(commitment_body()))
            .expect(1)
            .mount(&fast)
            .await;

        Mock::given(method("POST"))
            .and(path("/ledger/prepare-batch"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(StdDuration::from_millis(300))
                    .set_body_json(preview_body(5)),
            )
            .mount(&slow)
            .await;
        // The slow host must never see a finalize call at all — proves
        // this client never finalizes against more than one host.
        Mock::given(method("POST"))
            .and(path("/ledger/finalize-batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(commitment_body()))
            .expect(0)
            .mount(&slow)
            .await;

        let client = ManagedHostingClient::new(vec![
            HostCandidate {
                url: slow.uri(),
                submit_key: "k".to_string(),
            },
            HostCandidate {
                url: fast.uri(),
                submit_key: "k".to_string(),
            },
        ]);
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);

        let result = client
            .submit(&sample_batch(), &signing_key, "test-key")
            .await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn falls_back_to_the_other_candidate_when_the_first_hosts_finalize_fails() {
        let flaky = MockServer::start().await;
        let healthy = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ledger/prepare-batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(preview_body(5)))
            .mount(&flaky)
            .await;
        Mock::given(method("POST"))
            .and(path("/ledger/finalize-batch"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&flaky)
            .await;

        Mock::given(method("POST"))
            .and(path("/ledger/prepare-batch"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(StdDuration::from_millis(50))
                    .set_body_json(preview_body(5)),
            )
            .mount(&healthy)
            .await;
        Mock::given(method("POST"))
            .and(path("/ledger/finalize-batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(commitment_body()))
            .mount(&healthy)
            .await;

        let client = ManagedHostingClient::new(vec![
            HostCandidate {
                url: flaky.uri(),
                submit_key: "k".to_string(),
            },
            HostCandidate {
                url: healthy.uri(),
                submit_key: "k".to_string(),
            },
        ]);
        let signing_key = SigningKey::from_bytes(&[9u8; 32]);

        let result = client
            .submit(&sample_batch(), &signing_key, "test-key")
            .await;
        assert!(
            result.is_ok(),
            "expected fallback to the healthy host to succeed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn every_candidate_failing_prepare_surfaces_all_prepare_failed() {
        let dead = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ledger/prepare-batch"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&dead)
            .await;

        let client = ManagedHostingClient::new(vec![HostCandidate {
            url: dead.uri(),
            submit_key: "k".to_string(),
        }]);
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);

        let result = client
            .submit(&sample_batch(), &signing_key, "test-key")
            .await;
        assert!(matches!(
            result,
            Err(ManagedHostingError::AllPrepareFailed(_))
        ));
    }

    #[tokio::test]
    async fn no_candidates_is_a_clear_error_not_a_hang() {
        let client = ManagedHostingClient::new(vec![]);
        let signing_key = SigningKey::from_bytes(&[1u8; 32]);
        let result = client
            .submit(&sample_batch(), &signing_key, "test-key")
            .await;
        assert!(matches!(result, Err(ManagedHostingError::NoCandidates)));
    }
}
