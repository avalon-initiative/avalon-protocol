//! Checks that a configured node answers, sees its network and serves a head that verifies
//! against the network's pinned key.

use std::time::{Duration, Instant};

use avalon_protocol::sth::{verify_tree_head, SignedTreeHead};
use ed25519_dalek::VerifyingKey;
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverSummary {
    pub network_id: String,
    pub roles: Vec<String>,
    pub peers: usize,
    pub network_matches: bool,
}

/// Reads `GET /nodes/discover`.
pub fn summarize_discover(body: &Value, expected_network: &str) -> Result<DiscoverSummary, String> {
    let status = body
        .get("self_status")
        .ok_or("discover response has no self_status")?;
    let network_id = status
        .get("network_id")
        .and_then(Value::as_str)
        .ok_or("discover response has no network_id")?
        .to_string();
    let roles = status
        .get("roles")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let peers = body
        .get("peers")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Ok(DiscoverSummary {
        network_matches: network_id == expected_network,
        network_id,
        roles,
        peers,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadCheck {
    Verified { tree_size: i64 },
    BadSignature,
    Malformed(String),
}

/// Verifies a `GET /ledger/sth/latest` body against a pinned verify key (lowercase hex).
pub fn check_head(body: &Value, verify_key_hex: &str) -> HeadCheck {
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    let parsed = (|| {
        let created_at = OffsetDateTime::parse(&text("created_at")?, &Rfc3339).ok()?;
        Some(SignedTreeHead {
            tree_size: body.get("tree_size")?.as_i64()?,
            root_hash: text("root_hash")?,
            network_id: text("network_id")?,
            signing_key_id: text("signing_key_id")?,
            signature: text("signature")?,
            created_at,
        })
    })();
    let Some(sth) = parsed else {
        return HeadCheck::Malformed("head is missing fields".to_string());
    };
    let key = hex::decode(verify_key_hex)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .and_then(|b| VerifyingKey::from_bytes(&b).ok());
    let Some(key) = key else {
        return HeadCheck::Malformed("pinned verify key is invalid".to_string());
    };
    if verify_tree_head(&key, &sth) {
        HeadCheck::Verified {
            tree_size: sth.tree_size,
        }
    } else {
        HeadCheck::BadSignature
    }
}

/// Polls `GET /nodes/discover` until the node answers or `timeout` passes.
pub async fn wait_for_node(base: &str, timeout: Duration) -> Result<Value, String> {
    let client = client();
    let deadline = Instant::now() + timeout;
    let mut last = String::from("no answer");
    while Instant::now() < deadline {
        match client.get(format!("{base}/nodes/discover")).send().await {
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<Value>().await {
                    return Ok(v);
                }
                last = "unreadable response".to_string();
            }
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(format!(
        "{base} did not answer within {}s ({last})",
        timeout.as_secs()
    ))
}

/// Polls until the node knows at least one peer; returns the last discover body either way.
pub async fn wait_for_peers(
    base: &str,
    expected_network: &str,
    timeout: Duration,
) -> Option<DiscoverSummary> {
    let client = client();
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        if let Ok(r) = client.get(format!("{base}/nodes/discover")).send().await {
            if let Ok(v) = r.json::<Value>().await {
                if let Ok(s) = summarize_discover(&v, expected_network) {
                    if s.peers > 0 {
                        return Some(s);
                    }
                    last = Some(s);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    last
}

/// Polls the core head until one arrives and is judged, or `timeout` passes.
pub async fn wait_for_head(
    base: &str,
    verify_key_hex: &str,
    timeout: Duration,
) -> Option<HeadCheck> {
    let client = client();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(r) = client
            .get(format!("{base}/ledger/sth/latest?shard_id=core"))
            .send()
            .await
        {
            if r.status().is_success() {
                if let Ok(v) = r.json::<Value>().await {
                    return Some(check_head(&v, verify_key_hex));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    None
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("http client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::sth::sign_tree_head;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    #[test]
    fn discover_summary_counts_peers_and_flags_a_network_mismatch() {
        let body =
            json!({"self_status": {"network_id": "n1", "roles": ["combined"]}, "peers": [{}, {}]});
        let s = summarize_discover(&body, "n1").unwrap();
        assert_eq!((s.peers, s.network_matches), (2, true));
        assert!(!summarize_discover(&body, "other").unwrap().network_matches);
        assert!(summarize_discover(&json!({}), "n1").is_err());
    }

    fn head_json(key: &SigningKey, network: &str) -> Value {
        let created = OffsetDateTime::parse("2026-09-28T10:00:00Z", &Rfc3339).unwrap();
        let sth = sign_tree_head(key, "k1", 7, &"ab".repeat(32), network, created);
        json!({
            "tree_size": sth.tree_size, "root_hash": sth.root_hash, "network_id": sth.network_id,
            "signing_key_id": sth.signing_key_id, "signature": sth.signature,
            "created_at": "2026-09-28T10:00:00Z",
        })
    }

    #[test]
    fn head_verifies_against_the_pinned_key_only() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let other = SigningKey::from_bytes(&[4u8; 32]);
        let body = head_json(&key, "net");
        assert_eq!(
            check_head(&body, &hex::encode(key.verifying_key().to_bytes())),
            HeadCheck::Verified { tree_size: 7 }
        );
        assert_eq!(
            check_head(&body, &hex::encode(other.verifying_key().to_bytes())),
            HeadCheck::BadSignature
        );
    }

    #[test]
    fn malformed_head_is_reported_not_panicked_on() {
        assert!(matches!(
            check_head(&json!({"tree_size": 1}), &"00".repeat(32)),
            HeadCheck::Malformed(_)
        ));
    }
}
