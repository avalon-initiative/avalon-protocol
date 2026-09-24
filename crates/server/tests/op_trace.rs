//! Live checks for the opt-in `X-Avalon-Trace` header on real forwarded
//! operations: the realtime relay fan-out and the remote settlement submit.
//!
//! Run by `scripts/live-tests.sh` inside the `relay` group (tests named
//! `relay_*`) and the `remote-settlement` group (tests named `submit_*`).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const TRACE: &str = "x-avalon-trace";
const HOPS: &str = "x-avalon-trace-hops";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

async fn pool(url: &str) -> PgPool {
    PgPoolOptions::new()
        .connect(url)
        .await
        .expect("failed to connect to Postgres")
}

async fn seed_session(pool: &PgPool) -> String {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("op-trace-test-{identity_id}"))
        .execute(pool)
        .await
        .unwrap();
    let token = format!("test-token-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(time::OffsetDateTime::now_utc() + time::Duration::hours(1))
        .execute(pool)
        .await
        .unwrap();
    token
}

fn decode(resp: &reqwest::Response) -> Option<serde_json::Value> {
    let v = resp.headers().get(HOPS)?.to_str().ok()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(v).ok()?).ok()
}

fn header_names(resp: &reqwest::Response) -> Vec<String> {
    let mut names: Vec<String> = resp
        .headers()
        .keys()
        .map(|k| k.as_str().to_string())
        .filter(|k| k != "date")
        .collect();
    names.sort();
    names.dedup();
    names
}

#[tokio::test]
#[ignore]
async fn relay_traced_presence_reports_one_path_per_target() {
    let http = reqwest::Client::new();
    let a = env("AVALON_SERVER_URL");
    let b = env("AVALON_REALTIME_RELAY_PEER_SERVER_URL");
    let c = env("AVALON_MIRROR_PUSH_NO_DHT_PEER_SERVER_URL");
    let token = seed_session(&pool(&env("DATABASE_URL")).await).await;

    let put = |trace: Option<String>| {
        let mut r = http
            .put(format!("{a}/me/presence"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "status": "Away" }));
        if let Some(t) = trace {
            r = r.header(TRACE, t);
        }
        r.send()
    };

    let untraced = put(None).await.unwrap();
    assert!(untraced.status().is_success());
    assert!(untraced.headers().get(HOPS).is_none());
    let untraced_names = header_names(&untraced);
    let untraced_body: serde_json::Value = untraced.json().await.unwrap();

    // Peer tables fill on the announce interval; retry until both peers are known.
    let mut trace = None;
    let mut id = Uuid::nil();
    for _ in 0..40 {
        id = Uuid::new_v4();
        let resp = put(Some(id.to_string())).await.unwrap();
        assert!(resp.status().is_success());
        let t = decode(&resp).expect("traced relay must return the hops header");
        let ok = t["branches"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|br| br["outcome"] == "ok")
            .count();
        if ok >= 2 {
            let mut expected = untraced_names.clone();
            expected.push(HOPS.to_string());
            expected.sort();
            assert_eq!(
                header_names(&resp),
                expected,
                "only the trace header is added"
            );
            let body: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(
                body.as_object().unwrap().keys().collect::<Vec<_>>(),
                untraced_body
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>()
            );
            trace = Some(t);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let t = trace.expect("relay never fanned out to both peers");
    assert_eq!(t["trace_id"], id.to_string());
    assert_eq!(t["truncated"], false);
    for target in [&b, &c] {
        let branch = t["branches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|br| br["target"] == target.as_str())
            .unwrap_or_else(|| panic!("no branch for {target}: {t}"));
        assert_eq!(branch["outcome"], "ok");
        let hops = branch["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 2, "origin and one target per branch: {branch}");
        assert_eq!(hops[0]["base_url"], a);
        assert_eq!(hops[0]["index"], 0);
        assert!(hops[0]["to_next_ms"].as_f64().unwrap() >= 0.0);
        assert_eq!(hops[1]["base_url"], target.as_str());
        assert_eq!(hops[1]["index"], 1);
        assert!(hops[1].get("to_next_ms").is_none());
    }
    // Stale peer entries left by other tests show up as failed branches with
    // only the origin hop.
    for branch in t["branches"].as_array().unwrap() {
        if branch["outcome"] != "ok" {
            assert_eq!(branch["hops"].as_array().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
#[ignore]
async fn submit_traced_remote_submit_returns_gateway_then_authority() {
    let http = reqwest::Client::new();
    let authority = env("AVALON_SERVER_URL");
    let remote = env("AVALON_REMOTE_SETTLEMENT_SERVER_URL");
    let token = seed_session(&pool(&env("AVALON_REMOTE_SETTLEMENT_DATABASE_URL")).await).await;

    let create = |trace: Option<String>| {
        let suffix = Uuid::new_v4().simple().to_string();
        let mut r = http
            .post(format!("{remote}/guilds"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "name": format!("Op Trace {}", &suffix[..8]),
                "tag": suffix[..5].to_uppercase(),
                "description": "op trace live test",
            }));
        if let Some(t) = trace {
            r = r.header(TRACE, t);
        }
        r.send()
    };

    let untraced = create(None).await.unwrap();
    assert!(untraced.status().is_success());
    assert!(untraced.headers().get(HOPS).is_none());
    let untraced_names = header_names(&untraced);

    let id = Uuid::new_v4();
    let traced = create(Some(id.to_string())).await.unwrap();
    assert!(traced.status().is_success());
    assert_eq!(
        header_names(&traced),
        untraced_names,
        "the write response is unchanged"
    );

    let mut found = None;
    for _ in 0..40 {
        let status = http
            .get(format!("{remote}/ledger/remote-submit-status"))
            .header(TRACE, id.to_string())
            .send()
            .await
            .unwrap();
        if let Some(t) = decode(&status) {
            found = Some(t);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
    }
    let t = found.expect("the submit path was never recorded");
    let branches = t["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 1);
    let hops = branches[0]["hops"].as_array().unwrap();
    assert_eq!(hops.len(), 2, "{t}");
    assert_eq!(hops[0]["base_url"], remote);
    assert_eq!(hops[1]["base_url"], authority);
    assert!(hops[0]["to_next_ms"].as_f64().unwrap() >= 0.0);

    let plain = http
        .get(format!("{remote}/ledger/remote-submit-status"))
        .send()
        .await
        .unwrap();
    assert!(plain.headers().get(HOPS).is_none());
}
