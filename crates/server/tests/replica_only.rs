//! Live checks for a replica-only node. Gated `--ignored`: start a primary that authors
//! `core`, then a second node with `AVALON_REPLICA_ONLY=true`, no signing key and
//! `AVALON_MIRROR_PEERS` pointing at the primary.
//!
//! ```text
//! AVALON_PRIMARY_URL=http://127.0.0.1:35001 AVALON_REPLICA_URL=http://127.0.0.1:35002 \
//!   cargo test -p avalon-server --test replica_only -- --ignored
//! ```

fn urls() -> Option<(String, String)> {
    Some((
        std::env::var("AVALON_PRIMARY_URL").ok()?,
        std::env::var("AVALON_REPLICA_URL").ok()?,
    ))
}

#[tokio::test]
#[ignore]
async fn replica_serves_reads_appears_in_discovery_and_refuses_writes() {
    let Some((primary, replica)) = urls() else {
        eprintln!("skipping: AVALON_PRIMARY_URL/AVALON_REPLICA_URL not set");
        return;
    };
    let http = reqwest::Client::new();

    let status: serde_json::Value = http
        .get(format!("{replica}/nodes/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status["own_shard_replication"]["eligible_for_new_registrations"],
        false
    );

    // The replica serves core's head from its mirror, never from local signing.
    let primary_sth: serde_json::Value = http
        .get(format!("{primary}/ledger/sth/latest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut mirrored = None;
    for _ in 0..180 {
        let resp = http
            .get(format!("{replica}/ledger/sth/latest"))
            .send()
            .await
            .unwrap();
        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap();
            if body["root_hash"] == primary_sth["root_hash"] {
                mirrored = Some(body);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let mirrored = mirrored.expect("replica never served the primary's core head");
    assert_eq!(mirrored["signing_key_id"], primary_sth["signing_key_id"]);

    let mut listed = false;
    for _ in 0..180 {
        let peers: Vec<serde_json::Value> = http
            .get(format!("{primary}/nodes/peers"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if peers.iter().any(|p| p["base_url"] == replica.as_str()) {
            listed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(listed, "primary never listed the replica in discovery");

    let resp = http
        .post(format!("{replica}/identities/register/start"))
        .json(&serde_json::json!({
            "identity_id": uuid::Uuid::new_v4(),
            "display_name": "replica-refusal-check",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "REPLICA_ONLY");
}
