//! Gathers witness cosignatures for a head whose author signature already
//! verified, so a verifier can reach majority without depending on the head's
//! source having collected them.
//!
//! Each confirmed known-list witness is asked directly for its own
//! cosignature over the exact head; only cosignatures that verify against the
//! known-list key and match the head's root, size and creation time count.

use std::time::Duration;

use avalon_protocol::cosigned_sth::{self, CosignedTreeHead};
use avalon_protocol::sth::SignedTreeHead;
use avalon_protocol::witness::WitnessCosignature;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::cosign_verify::{WitnessCosignatureDto, COSIGNATURE_FRESHNESS_WINDOW};
use crate::nodes::PeerInfo;
use crate::outbound_policy::OutboundPolicy;

/// Upper bound on simultaneous witness fetches for one head.
pub const MAX_CONCURRENT_WITNESS_FETCHES: usize = 8;
/// Per-witness request timeout.
pub const WITNESS_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// A confirmed known-list witness together with the base URL it is reachable at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessSource {
    pub key_id: String,
    pub base_url: String,
}

/// Outcome of trying to reach majority for an author-verified head.
#[derive(Debug)]
pub enum HeadVerdict {
    /// Majority reached (or not required); carries the head with every
    /// accepted cosignature merged in.
    Trusted(CosignedTreeHead),
    /// Author signature is valid but majority is not reached yet.
    Held,
}

/// Resolves each known-list witness to the peer-table entry whose directly
/// verified witness advert carries that key. Witnesses with no such entry are
/// left out.
pub fn witness_sources(
    known_list: &[(String, VerifyingKey)],
    peers: &[PeerInfo],
) -> Vec<WitnessSource> {
    known_list
        .iter()
        .filter_map(|(key_id, _)| {
            let peer = peers.iter().find(|p| {
                p.witness
                    .as_ref()
                    .is_some_and(|w| w.direct && w.key_id == *key_id)
            })?;
            Some(WitnessSource {
                key_id: key_id.clone(),
                base_url: crate::node_http::NodeClient::url_for(peer),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct WitnessSthResponse {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(default)]
    cosignatures: Vec<WitnessCosignatureDto>,
}

/// What asking one witness for its own cosignature produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatherOutcome {
    /// The witness's own cosignature, verified against its known-list key for the exact head.
    Fetched(WitnessCosignature),
    /// No peer-table entry with a directly verified advert for this witness.
    NoSource,
    /// The outbound policy refused the witness's base URL.
    Blocked,
    /// The request failed before a complete response arrived.
    Unreachable(String),
    /// The witness answered with a non-success status.
    BadStatus(u16),
    /// The response was oversized or not a well-formed tree head document.
    Malformed,
    /// The witness holds a different head at this tree size.
    HeadMismatch,
    /// The response carried no cosignature by the asked witness itself.
    NoOwnCosignature,
    /// The witness's own cosignature did not verify for this head.
    InvalidSignature,
}

impl GatherOutcome {
    /// Short label used to log only when a witness's outcome changes.
    pub fn label(&self) -> String {
        match self {
            Self::Fetched(_) => "fetched".into(),
            Self::NoSource => "no_source".into(),
            Self::Blocked => "blocked".into(),
            Self::Unreachable(e) => format!("unreachable: {e}"),
            Self::BadStatus(s) => format!("status {s}"),
            Self::Malformed => "malformed".into(),
            Self::HeadMismatch => "head_mismatch".into(),
            Self::NoOwnCosignature => "no_own_cosignature".into(),
            Self::InvalidSignature => "invalid_signature".into(),
        }
    }
}

/// Asks one witness for its own cosignature over `sth`. Only a cosignature by the asked witness
/// counts: relayed copies of other witnesses' cosignatures it holds may be stale and would
/// otherwise displace their fresh ones when merged.
async fn fetch_own_cosignature(
    policy: OutboundPolicy,
    source: &WitnessSource,
    sth: &SignedTreeHead,
    shard_id: &str,
) -> GatherOutcome {
    let Ok(target) = policy
        .check_node_url_for(
            &source.base_url,
            crate::outbound_policy::LookupPurpose::Critical,
        )
        .await
    else {
        return GatherOutcome::Blocked;
    };
    let client = target.node_client(WITNESS_FETCH_TIMEOUT);
    let url = format!("{}/ledger/sth/{}", target.base_url, sth.tree_size);
    let mut response = match client
        .get(url)
        .query(&[("shard_id", shard_id), ("witnesses", "1")])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return GatherOutcome::Unreachable(e.without_url().to_string()),
    };
    if !response.status().is_success() {
        return GatherOutcome::BadStatus(response.status().as_u16());
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_RESPONSE_BYTES {
                    return GatherOutcome::Malformed;
                }
            }
            Ok(None) => break,
            Err(e) => return GatherOutcome::Unreachable(e.without_url().to_string()),
        }
    }
    let Ok(parsed) = serde_json::from_slice::<WitnessSthResponse>(&body) else {
        return GatherOutcome::Malformed;
    };
    if parsed.tree_size != sth.tree_size
        || parsed.root_hash != sth.root_hash
        || parsed.network_id != sth.network_id
        || parsed.created_at != sth.created_at
    {
        return GatherOutcome::HeadMismatch;
    }
    match parsed
        .cosignatures
        .iter()
        .find(|c| c.witness_key_id == source.key_id)
    {
        Some(c) => GatherOutcome::Fetched(c.to_witness_cosignature(sth)),
        None => GatherOutcome::NoOwnCosignature,
    }
}

/// Asks every known-list witness for its own current cosignature over `sth`, with bounded
/// concurrency. One entry per known-list witness; a `Fetched` entry has already verified against
/// that witness's known-list key for exactly `sth`.
pub async fn gather_own_cosignatures(
    policy: OutboundPolicy,
    known_list: &[(String, VerifyingKey)],
    sources: &[WitnessSource],
    sth: &SignedTreeHead,
    shard_id: &str,
) -> Vec<(String, GatherOutcome)> {
    let mut results = Vec::with_capacity(known_list.len());
    let mut asked: Vec<&WitnessSource> = Vec::new();
    for (key_id, _) in known_list {
        match sources.iter().find(|s| s.key_id == *key_id) {
            Some(source) => asked.push(source),
            None => results.push((key_id.clone(), GatherOutcome::NoSource)),
        }
    }
    for batch in asked.chunks(MAX_CONCURRENT_WITNESS_FETCHES) {
        let outcomes = futures_util::future::join_all(
            batch
                .iter()
                .map(|s| fetch_own_cosignature(policy, s, sth, shard_id)),
        )
        .await;
        for (source, outcome) in batch.iter().zip(outcomes) {
            let outcome = match outcome {
                GatherOutcome::Fetched(cosig) => {
                    let valid = known_list
                        .iter()
                        .find(|(id, _)| *id == source.key_id)
                        .is_some_and(|(_, key)| {
                            avalon_protocol::witness::verify_witness_cosignature(key, &cosig)
                        });
                    if valid {
                        GatherOutcome::Fetched(cosig)
                    } else {
                        GatherOutcome::InvalidSignature
                    }
                }
                other => other,
            };
            results.push((source.key_id.clone(), outcome));
        }
    }
    results
}

/// Fetches cosignatures for `head` from every source whose cosignature is absent from
/// `head.cosignatures` or older than the freshness window, with bounded concurrency. Failures
/// yield nothing.
pub async fn gather_witness_cosignatures(
    policy: OutboundPolicy,
    sources: &[WitnessSource],
    head: &CosignedTreeHead,
    shard_id: &str,
) -> Vec<WitnessCosignature> {
    let cutoff = OffsetDateTime::now_utc() - COSIGNATURE_FRESHNESS_WINDOW;
    let wanted: Vec<&WitnessSource> = sources
        .iter()
        .filter(|s| {
            !head
                .cosignatures
                .iter()
                .any(|c| c.witness_key_id == s.key_id && c.observed_at >= cutoff)
        })
        .collect();
    let mut gathered = Vec::new();
    for batch in wanted.chunks(MAX_CONCURRENT_WITNESS_FETCHES) {
        let results = futures_util::future::join_all(
            batch
                .iter()
                .map(|s| fetch_own_cosignature(policy, s, &head.sth, shard_id)),
        )
        .await;
        gathered.extend(results.into_iter().filter_map(|o| match o {
            GatherOutcome::Fetched(c) => Some(c),
            _ => None,
        }));
    }
    gathered
}

/// Adds `extra` to `head`; for a witness already present, the cosignature with the later
/// `observed_at` is kept.
pub fn merge_cosignatures(
    mut head: CosignedTreeHead,
    extra: Vec<WitnessCosignature>,
) -> CosignedTreeHead {
    for cosig in extra {
        match head
            .cosignatures
            .iter_mut()
            .find(|c| c.witness_key_id == cosig.witness_key_id)
        {
            Some(existing) if existing.observed_at < cosig.observed_at => *existing = cosig,
            Some(_) => {}
            None => head.cosignatures.push(cosig),
        }
    }
    head
}

/// Decides whether an author-verified `head` is trusted: with a known list of
/// at most one witness the author signature is enough; otherwise majority is
/// checked on the cosignatures already attached and, if short, again after
/// gathering from `sources`.
pub async fn resolve_majority(
    policy: OutboundPolicy,
    author_key: &VerifyingKey,
    head: CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
    sources: &[WitnessSource],
    shard_id: &str,
) -> HeadVerdict {
    let now = OffsetDateTime::now_utc();
    let cutoff = now - COSIGNATURE_FRESHNESS_WINDOW;
    if cosigned_sth::verify_cosigned_tree_head(author_key, &head, known_list, cutoff, now) {
        return HeadVerdict::Trusted(head);
    }
    let extra = gather_witness_cosignatures(policy, sources, &head, shard_id).await;
    let merged = merge_cosignatures(head, extra);
    let now = OffsetDateTime::now_utc();
    if cosigned_sth::verify_cosigned_tree_head(
        author_key,
        &merged,
        known_list,
        now - COSIGNATURE_FRESHNESS_WINDOW,
        now,
    ) {
        HeadVerdict::Trusted(merged)
    } else {
        HeadVerdict::Held
    }
}

/// Author-checks `head` against every candidate key, then decides majority as
/// [`resolve_majority`] does. `None` when no key verifies the author signature
/// or majority is not reached; for callers that only verify and never cosign.
pub async fn verify_with_gathering(
    policy: OutboundPolicy,
    candidate_author_keys: impl IntoIterator<Item = VerifyingKey>,
    head: CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
    sources: &[WitnessSource],
    shard_id: &str,
) -> Option<CosignedTreeHead> {
    let now = OffsetDateTime::now_utc();
    let author_key = crate::cosign_verify::verify_cosigned_against_any_key(
        candidate_author_keys,
        &head,
        &[],
        now,
    )?;
    match resolve_majority(policy, &author_key, head, known_list, sources, shard_id).await {
        HeadVerdict::Trusted(head) => Some(head),
        HeadVerdict::Held => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::sth::{sign_tree_head, SignedTreeHead};
    use avalon_protocol::witness::sign_witness_cosignature;
    use ed25519_dalek::SigningKey;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn author() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    fn head(root_byte: u8) -> CosignedTreeHead {
        let created_at = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
        let sth = sign_tree_head(
            &author(),
            "op",
            5,
            &hex::encode([root_byte; 32]),
            "net",
            created_at,
        )
        .unwrap();
        CosignedTreeHead {
            sth,
            cosignatures: Vec::new(),
        }
    }

    fn witness(seed: u8) -> (SigningKey, String) {
        let k = SigningKey::from_bytes(&[seed; 32]);
        let id = hex::encode(k.verifying_key().to_bytes());
        (k, id)
    }

    fn cosign(k: &SigningKey, id: &str, sth: &SignedTreeHead) -> WitnessCosignature {
        sign_witness_cosignature(
            k,
            id,
            sth.tree_size,
            &sth.root_hash,
            &sth.network_id,
            sth.created_at,
            OffsetDateTime::now_utc(),
        )
    }

    fn body(sth: &SignedTreeHead, cosigs: &[WitnessCosignature]) -> serde_json::Value {
        serde_json::json!({
            "tree_size": sth.tree_size,
            "root_hash": sth.root_hash,
            "network_id": sth.network_id,
            "signing_key_id": sth.signing_key_id,
            "signature": sth.signature,
            "created_at": sth.created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
            "cosignatures": cosigs.iter().map(WitnessCosignatureDto::from_witness_cosignature).collect::<Vec<_>>(),
        })
    }

    async fn serve(server: &MockServer, template: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/ledger/sth/5"))
            .and(query_param("witnesses", "1"))
            .respond_with(template)
            .mount(server)
            .await;
    }

    fn policy() -> OutboundPolicy {
        OutboundPolicy::new(true)
    }

    fn known(ws: &[&(SigningKey, String)]) -> Vec<(String, VerifyingKey)> {
        ws.iter()
            .map(|(k, id)| (id.clone(), k.verifying_key()))
            .collect()
    }

    fn source(id: &str, server: &MockServer) -> WitnessSource {
        WitnessSource {
            key_id: id.to_string(),
            base_url: server.uri(),
        }
    }

    #[tokio::test]
    async fn majority_is_reached_only_after_gathering() {
        let (w1, w2, w3) = (witness(1), witness(2), witness(3));
        let h = head(1);
        let (s1, s2) = (MockServer::start().await, MockServer::start().await);
        serve(
            &s1,
            ResponseTemplate::new(200).set_body_json(body(&h.sth, &[cosign(&w1.0, &w1.1, &h.sth)])),
        )
        .await;
        serve(
            &s2,
            ResponseTemplate::new(200).set_body_json(body(&h.sth, &[cosign(&w2.0, &w2.1, &h.sth)])),
        )
        .await;
        let list = known(&[&w1, &w2, &w3]);
        let sources = [
            source(&w1.1, &s1),
            source(&w2.1, &s2),
            WitnessSource {
                key_id: w3.1.clone(),
                base_url: "http://127.0.0.1:1".into(),
            },
        ];
        let verdict = resolve_majority(
            policy(),
            &author().verifying_key(),
            h,
            &list,
            &sources,
            "core",
        )
        .await;
        match verdict {
            HeadVerdict::Trusted(merged) => assert_eq!(merged.cosignatures.len(), 2),
            HeadVerdict::Held => panic!("expected majority after gathering"),
        }
    }

    #[tokio::test]
    async fn a_relayed_stale_copy_does_not_displace_the_witnesss_own_fresh_cosignature() {
        let (w1, w2) = (witness(1), witness(2));
        let h = head(1);
        let stale = sign_witness_cosignature(
            &w2.0,
            &w2.1,
            h.sth.tree_size,
            &h.sth.root_hash,
            &h.sth.network_id,
            h.sth.created_at,
            OffsetDateTime::now_utc() - time::Duration::hours(1),
        );
        let (s1, s2) = (MockServer::start().await, MockServer::start().await);
        // The first witness holds its own fresh cosignature plus an old copy of the second's.
        serve(
            &s1,
            ResponseTemplate::new(200)
                .set_body_json(body(&h.sth, &[cosign(&w1.0, &w1.1, &h.sth), stale])),
        )
        .await;
        serve(
            &s2,
            ResponseTemplate::new(200).set_body_json(body(&h.sth, &[cosign(&w2.0, &w2.1, &h.sth)])),
        )
        .await;
        let list = known(&[&w1, &w2]);
        let sources = [source(&w1.1, &s1), source(&w2.1, &s2)];
        let verdict = resolve_majority(
            policy(),
            &author().verifying_key(),
            h,
            &list,
            &sources,
            "core",
        )
        .await;
        assert!(matches!(verdict, HeadVerdict::Trusted(_)));
    }

    fn stale_cosign(k: &SigningKey, id: &str, sth: &SignedTreeHead) -> WitnessCosignature {
        sign_witness_cosignature(
            k,
            id,
            sth.tree_size,
            &sth.root_hash,
            &sth.network_id,
            sth.created_at,
            OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
    }

    #[test]
    fn merge_keeps_the_later_observation_per_witness() {
        let w1 = witness(1);
        let mut h = head(1);
        let old = stale_cosign(&w1.0, &w1.1, &h.sth);
        let fresh = cosign(&w1.0, &w1.1, &h.sth);
        h.cosignatures.push(old.clone());
        let merged = merge_cosignatures(h, vec![fresh.clone()]);
        assert_eq!(merged.cosignatures, vec![fresh.clone()]);
        let merged = merge_cosignatures(merged, vec![old]);
        assert_eq!(merged.cosignatures, vec![fresh]);
    }

    #[tokio::test]
    async fn a_stale_attached_copy_does_not_stop_gathering_that_witness() {
        let (w1, w2) = (witness(1), witness(2));
        let mut h = head(1);
        h.cosignatures.push(stale_cosign(&w2.0, &w2.1, &h.sth));
        h.cosignatures.push(cosign(&w1.0, &w1.1, &h.sth));
        let s2 = MockServer::start().await;
        serve(
            &s2,
            ResponseTemplate::new(200).set_body_json(body(&h.sth, &[cosign(&w2.0, &w2.1, &h.sth)])),
        )
        .await;
        let sources = [
            WitnessSource {
                key_id: w1.1.clone(),
                base_url: "http://127.0.0.1:1".into(),
            },
            source(&w2.1, &s2),
        ];
        let got = gather_witness_cosignatures(policy(), &sources, &h, "core").await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].witness_key_id, w2.1);
        let cutoff = OffsetDateTime::now_utc() - COSIGNATURE_FRESHNESS_WINDOW;
        assert!(got[0].observed_at >= cutoff);
    }

    #[tokio::test]
    async fn own_cosignatures_report_a_per_witness_outcome() {
        let (w1, w2, w3, w4) = (witness(1), witness(2), witness(3), witness(4));
        let h = head(1);
        let (s1, s2, s3) = (
            MockServer::start().await,
            MockServer::start().await,
            MockServer::start().await,
        );
        // Relayed copy of w2's cosignature is ignored; w1's own is returned.
        serve(
            &s1,
            ResponseTemplate::new(200).set_body_json(body(
                &h.sth,
                &[
                    stale_cosign(&w2.0, &w2.1, &h.sth),
                    cosign(&w1.0, &w1.1, &h.sth),
                ],
            )),
        )
        .await;
        serve(&s2, ResponseTemplate::new(503)).await;
        // w3's server answers with a cosignature by w1 only.
        serve(
            &s3,
            ResponseTemplate::new(200).set_body_json(body(&h.sth, &[cosign(&w1.0, &w1.1, &h.sth)])),
        )
        .await;
        let list = known(&[&w1, &w2, &w3, &w4]);
        let sources = [source(&w1.1, &s1), source(&w2.1, &s2), source(&w3.1, &s3)];
        let out = gather_own_cosignatures(policy(), &list, &sources, &h.sth, "core").await;
        let get = |id: &str| out.iter().find(|(k, _)| k == id).unwrap().1.clone();
        match get(&w1.1) {
            GatherOutcome::Fetched(c) => assert_eq!(c.witness_key_id, w1.1),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(get(&w2.1), GatherOutcome::BadStatus(503));
        assert_eq!(get(&w3.1), GatherOutcome::NoOwnCosignature);
        assert_eq!(get(&w4.1), GatherOutcome::NoSource);
    }

    #[tokio::test]
    async fn head_is_held_when_majority_is_not_reached() {
        let (w1, w2, w3) = (witness(1), witness(2), witness(3));
        let h = head(1);
        let s1 = MockServer::start().await;
        serve(
            &s1,
            ResponseTemplate::new(200).set_body_json(body(&h.sth, &[cosign(&w1.0, &w1.1, &h.sth)])),
        )
        .await;
        let s2 = MockServer::start().await;
        serve(&s2, ResponseTemplate::new(404)).await;
        let list = known(&[&w1, &w2, &w3]);
        let sources = [source(&w1.1, &s1), source(&w2.1, &s2)];
        let verdict = resolve_majority(
            policy(),
            &author().verifying_key(),
            h,
            &list,
            &sources,
            "core",
        )
        .await;
        assert!(matches!(verdict, HeadVerdict::Held));
    }

    #[tokio::test]
    async fn cosignatures_over_a_different_head_are_ignored() {
        let (w1, w2) = (witness(1), witness(2));
        let h = head(1);
        let other = head(2);
        let (s1, s2) = (MockServer::start().await, MockServer::start().await);
        // Right cosignature envelope but bound to another root, and a response
        // that claims another root outright.
        serve(
            &s1,
            ResponseTemplate::new(200)
                .set_body_json(body(&other.sth, &[cosign(&w1.0, &w1.1, &other.sth)])),
        )
        .await;
        let mut lying = body(&h.sth, &[cosign(&w2.0, &w2.1, &other.sth)]);
        lying["cosignatures"][0]["signature"] = serde_json::json!("00");
        serve(&s2, ResponseTemplate::new(200).set_body_json(lying)).await;
        let list = known(&[&w1, &w2]);
        let sources = [source(&w1.1, &s1), source(&w2.1, &s2)];
        let verdict = resolve_majority(
            policy(),
            &author().verifying_key(),
            h,
            &list,
            &sources,
            "core",
        )
        .await;
        assert!(matches!(verdict, HeadVerdict::Held));
    }

    #[tokio::test]
    async fn unreachable_and_garbage_witnesses_do_not_panic() {
        let (w1, w2, w3) = (witness(1), witness(2), witness(3));
        let h = head(1);
        let s2 = MockServer::start().await;
        serve(&s2, ResponseTemplate::new(200).set_body_string("not json")).await;
        let list = known(&[&w1, &w2, &w3]);
        let sources = [
            WitnessSource {
                key_id: w1.1.clone(),
                base_url: "http://127.0.0.1:1".into(),
            },
            source(&w2.1, &s2),
            WitnessSource {
                key_id: w3.1.clone(),
                base_url: "not a url".into(),
            },
        ];
        let verdict = resolve_majority(
            policy(),
            &author().verifying_key(),
            h,
            &list,
            &sources,
            "core",
        )
        .await;
        assert!(matches!(verdict, HeadVerdict::Held));
    }

    #[tokio::test]
    async fn a_known_list_of_one_needs_no_gathering() {
        let w1 = witness(1);
        let h = head(1);
        let list = known(&[&w1]);
        let verdict =
            resolve_majority(policy(), &author().verifying_key(), h, &list, &[], "core").await;
        assert!(matches!(verdict, HeadVerdict::Trusted(_)));
    }

    #[test]
    fn sources_resolve_only_through_direct_adverts() {
        use crate::nodes::WitnessAdvert;
        let (w1, w2) = (witness(1), witness(2));
        let now = OffsetDateTime::now_utc();
        let mk = |url: &str, id: &str, direct: bool| PeerInfo {
            identity_bound: false,
            base_url: url.into(),
            roles: vec![],
            protocol_version: String::new(),
            network_id: "net".into(),
            last_announced_at: now,
            libp2p_peer_id: None,
            libp2p_listen_addrs: vec![],
            connectivity: None,
            witness: Some(WitnessAdvert {
                key_id: id.into(),
                announced_at: now,
                proof: String::new(),
                direct,
            }),
        };
        let peers = [mk("http://a", &w1.1, true), mk("http://b", &w2.1, false)];
        let list = known(&[&w1, &w2]);
        let sources = witness_sources(&list, &peers);
        assert_eq!(
            sources,
            vec![WitnessSource {
                key_id: w1.1.clone(),
                base_url: "http://a".into()
            }]
        );
    }
}
