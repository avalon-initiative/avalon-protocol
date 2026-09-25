//! Gathers witness cosignatures for a head whose author signature already
//! verified, so a verifier can reach majority without depending on the head's
//! source having collected them.
//!
//! Each confirmed known-list witness is asked directly for its own
//! cosignature over the exact head; only cosignatures that verify against the
//! known-list key and match the head's root, size and creation time count.

use std::time::Duration;

use avalon_protocol::cosigned_sth::{self, CosignedTreeHead};
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
                base_url: peer.base_url.clone(),
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

async fn fetch_from_witness(
    policy: OutboundPolicy,
    source: &WitnessSource,
    head: &CosignedTreeHead,
    shard_id: &str,
) -> Vec<WitnessCosignature> {
    let Ok(target) = policy.check_base_url(&source.base_url).await else {
        return Vec::new();
    };
    let client = target.client(WITNESS_FETCH_TIMEOUT);
    let url = format!("{}/ledger/sth/{}", target.base_url, head.sth.tree_size);
    let Ok(mut response) = client
        .get(url)
        .query(&[("shard_id", shard_id), ("witnesses", "1")])
        .send()
        .await
    else {
        return Vec::new();
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_RESPONSE_BYTES {
            return Vec::new();
        }
    }
    let Ok(parsed) = serde_json::from_slice::<WitnessSthResponse>(&body) else {
        return Vec::new();
    };
    if parsed.tree_size != head.sth.tree_size
        || parsed.root_hash != head.sth.root_hash
        || parsed.network_id != head.sth.network_id
        || parsed.created_at != head.sth.created_at
    {
        return Vec::new();
    }
    parsed
        .cosignatures
        .iter()
        .map(|c| c.to_witness_cosignature(&head.sth))
        .collect()
}

/// Fetches cosignatures for `head` from every source not already represented
/// in `head.cosignatures`, with bounded concurrency. Failures yield nothing.
pub async fn gather_witness_cosignatures(
    policy: OutboundPolicy,
    sources: &[WitnessSource],
    head: &CosignedTreeHead,
    shard_id: &str,
) -> Vec<WitnessCosignature> {
    let wanted: Vec<&WitnessSource> = sources
        .iter()
        .filter(|s| {
            !head
                .cosignatures
                .iter()
                .any(|c| c.witness_key_id == s.key_id)
        })
        .collect();
    let mut gathered = Vec::new();
    for batch in wanted.chunks(MAX_CONCURRENT_WITNESS_FETCHES) {
        let results = futures_util::future::join_all(
            batch
                .iter()
                .map(|s| fetch_from_witness(policy, s, head, shard_id)),
        )
        .await;
        gathered.extend(results.into_iter().flatten());
    }
    gathered
}

/// Adds `extra` to `head`, skipping any witness id already present.
pub fn merge_cosignatures(
    mut head: CosignedTreeHead,
    extra: Vec<WitnessCosignature>,
) -> CosignedTreeHead {
    for cosig in extra {
        if !head
            .cosignatures
            .iter()
            .any(|c| c.witness_key_id == cosig.witness_key_id)
        {
            head.cosignatures.push(cosig);
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
        );
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
            base_url: url.into(),
            roles: vec![],
            protocol_version: String::new(),
            network_id: "net".into(),
            last_announced_at: now,
            libp2p_peer_id: None,
            libp2p_listen_addrs: vec![],
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
