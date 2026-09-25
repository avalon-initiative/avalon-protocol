//! The load scenarios. Each takes the limits the script configured the node
//! with as `--key value` arguments and asserts against them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use avalon_loadtest::procstat::Sampler;
use avalon_loadtest::stats::max_admitted;
use avalon_loadtest::target::alt_loopback;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::common::*;

const NO_THINK: Duration = Duration::ZERO;

fn get(c: &reqwest::Client, url: String) -> RequestFn {
    let c = c.clone();
    Arc::new(move |_| ("GET", c.get(&url)))
}

/// Requests admitted before the first 429 relative to the configured limit.
fn admitted_bounds(report: &mut Report, ok: u64, limit: u64, elapsed: Duration, slack_low: u64) {
    let hi = max_admitted(limit, limit, elapsed) + 2;
    report.metric("admitted", ok);
    report.metric("admitted_upper_bound", hi);
    report.check(
        ok <= hi,
        "admitted requests stay within burst plus refill",
        format!("{ok} <= {hi}"),
    );
    report.check(
        ok + slack_low >= limit,
        "the full burst was admitted before limiting",
        format!("{ok} + {slack_low} >= {limit}"),
    );
}

pub async fn flood_public(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("flood-public");
    let limit = ctx.num("limit", ctx.scaled(60, 600));
    let total = (limit * 3 / 2 + 20) as usize;
    let a = ctx.client(None);
    let (samples, elapsed) = run_load(
        16,
        Stop::Count(total),
        NO_THINK,
        get(&a, ctx.url("/nodes/status")),
    )
    .await;
    let t = tally(&samples);
    r.metric("limit_per_minute", limit);
    r.metric("requests", total);
    r.metric("statuses", t.render());
    r.metric("elapsed_ms", elapsed.as_millis() as u64);
    r.metric("rps", round(total as f64 / elapsed.as_secs_f64()));
    admitted_bounds(&mut r, t.ok(), limit, elapsed, 30);
    r.check(
        t.count(429) > 0,
        "requests past the ceiling get 429",
        format!("429s: {}", t.count(429)),
    );
    r.check(
        samples
            .iter()
            .filter(|s| s.status == 429)
            .all(|s| s.retry_after),
        "every 429 carries Retry-After",
        "",
    );
    r.check(
        t.server_errors() == 0,
        "no 5xx or transport errors while limiting",
        t.render(),
    );

    let spoofed = a
        .get(ctx.url("/nodes/status"))
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await?;
    r.check(
        spoofed.status().as_u16() == 429,
        "a spoofed X-Forwarded-For does not escape the per-address limit",
        format!("status {}", spoofed.status()),
    );
    let other = ctx
        .client(Some(alt_loopback(2)))
        .get(ctx.url("/nodes/status"))
        .send()
        .await?;
    r.check(
        other.status().is_success(),
        "a client at another address is unaffected",
        format!("status {}", other.status()),
    );
    Ok(r)
}

pub async fn flood_auth(ctx: &Ctx) -> Result<Report> {
    let mode = ctx
        .args
        .get("mode")
        .cloned()
        .unwrap_or_else(|| "principal".into());
    let mut r = Report::new(&format!("flood-auth-{mode}"));
    let limit = ctx.num("limit", ctx.scaled(60, 600));
    let ids = seed_identities(ctx.pool()?, 2).await?;
    let (tok_a, tok_b) = (ids[0].1.clone(), ids[1].1.clone());
    let total = (limit * 3 / 2 + 20) as usize;
    let c = ctx.client(None);
    let url = ctx.url("/me");
    let make: RequestFn = {
        let (c, url, tok) = (c.clone(), url.clone(), tok_a.clone());
        Arc::new(move |_| ("GET /me", c.get(&url).bearer_auth(&tok)))
    };
    let (samples, elapsed) = run_load(16, Stop::Count(total), NO_THINK, make).await;
    let t = tally(&samples);
    r.metric("mode", mode.clone());
    r.metric("limit_per_minute", limit);
    r.metric("statuses", t.render());
    r.metric("elapsed_ms", elapsed.as_millis() as u64);
    admitted_bounds(&mut r, t.ok(), limit, elapsed, 30);
    r.check(
        t.count(429) > 0,
        "requests past the limit get 429",
        format!("429s: {}", t.count(429)),
    );
    r.check(
        samples
            .iter()
            .filter(|s| s.status == 429)
            .all(|s| s.retry_after),
        "every 429 carries Retry-After",
        "",
    );
    r.check(
        t.server_errors() == 0,
        "no 5xx or transport errors while limiting",
        t.render(),
    );
    if mode == "principal" {
        let b = c.get(&url).bearer_auth(&tok_b).send().await?;
        r.check(
            b.status().is_success(),
            "a second identity from the same address is unaffected",
            format!("status {}", b.status()),
        );
    } else {
        let other = ctx
            .client(Some(alt_loopback(2)))
            .get(&url)
            .bearer_auth(&tok_a)
            .send()
            .await?;
        r.check(
            other.status().is_success(),
            "the same identity from another address is unaffected by the per-address ceiling",
            format!("status {}", other.status()),
        );
    }
    Ok(r)
}

pub async fn many_identities(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("many-identities");
    let limit = ctx.num("limit", ctx.scaled(60, 600));
    let total = (limit * 2 + 20) as usize;
    let per_identity = 4usize;
    let n = total.div_ceil(per_identity);
    let ids = seed_identities(ctx.pool()?, n).await?;
    let tokens: Arc<Vec<String>> = Arc::new(ids.into_iter().map(|(_, t)| t).collect());
    let c = ctx.client(None);
    let url = ctx.url("/me");
    let make: RequestFn = {
        let (c, url, tokens) = (c.clone(), url.clone(), tokens.clone());
        Arc::new(move |i| {
            (
                "GET /me",
                c.get(&url).bearer_auth(&tokens[i % tokens.len()]),
            )
        })
    };
    let (samples, elapsed) = run_load(16, Stop::Count(total), NO_THINK, make).await;
    let t = tally(&samples);
    r.metric("identities", n);
    r.metric("requests_per_identity", per_identity);
    r.metric("limit_per_minute", limit);
    r.metric("statuses", t.render());
    admitted_bounds(&mut r, t.ok(), limit, elapsed, 30);
    r.check(
        t.count(429) > 0,
        "the per-address ceiling bounds many identities from one address",
        format!("429s: {}", t.count(429)),
    );
    r.check(
        t.server_errors() == 0,
        "no 5xx or transport errors",
        t.render(),
    );
    Ok(r)
}

fn announce_body(info: &Value, url: &str) -> Value {
    json!({
        "base_url": url,
        "roles": ["combined"],
        "protocol_version": info["protocol_version"],
        "network_id": info["network_id"],
        "coordinate": {"vector": [0.0, 0.0, 0.0], "height": 0.01, "error": 1.0},
    })
}

fn fabricated_url(i: usize) -> String {
    let n = i + 1;
    format!(
        "http://10.{}.{}.{}:8080",
        (n >> 16) & 255,
        (n >> 8) & 255,
        n & 255
    )
}

async fn peer_urls(ctx: &Ctx, node: &str) -> Result<Vec<String>> {
    let v: Value = ctx
        .client(None)
        .get(format!("{node}/nodes/peers"))
        .send()
        .await?
        .json()
        .await?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p["base_url"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

pub async fn announce_flood(ctx: &Ctx) -> Result<Report> {
    let mode = ctx
        .args
        .get("mode")
        .cloned()
        .unwrap_or_else(|| "source".into());
    let mut r = Report::new(&format!("announce-flood-{mode}"));
    let info = ctx.node_info().await?;
    let base = ctx.base.as_str().trim_end_matches('/').to_string();
    let c = ctx.client(None);
    let post = |c: reqwest::Client, url: String, body: Value| async move {
        send_one("POST", c.post(url).json(&body)).await
    };
    match mode.as_str() {
        "source" => {
            let limit = ctx.num("source-limit", 10) as usize;
            let total = limit * 5;
            let mut samples = Vec::new();
            for i in 0..total {
                let body = announce_body(&info, &fabricated_url(i));
                samples.push(post(c.clone(), format!("{base}/nodes/announce"), body).await);
            }
            let t = tally(&samples);
            r.metric("source_limit_per_minute", limit);
            r.metric("distinct_new_urls_sent", total);
            r.metric("statuses", t.render());
            r.check(
                t.ok() == limit as u64,
                "exactly the per-source budget of new URLs is admitted",
                format!("admitted {}", t.ok()),
            );
            let limited: Vec<_> = samples.iter().filter(|s| s.status == 429).collect();
            r.check(
                !limited.is_empty(),
                "further new URLs get 429",
                format!("{}", limited.len()),
            );
            r.check(
                limited
                    .iter()
                    .all(|s| s.retry_after && s.code.as_deref() == Some("rate_limited")),
                "429s carry Retry-After and the rate_limited code",
                "",
            );
            let peers = peer_urls(ctx, &base).await?.len();
            r.metric("peer_table_size", peers);
            r.check(
                peers == limit,
                "the peer table holds only the admitted entries",
                format!("{peers}"),
            );
            let refresh = post(
                c.clone(),
                format!("{base}/nodes/announce"),
                announce_body(&info, &fabricated_url(0)),
            )
            .await;
            r.check(
                refresh.status == 200,
                "re-announcing a known URL is not counted against the budget",
                format!("status {}", refresh.status),
            );
            let other = post(
                ctx.client(Some(alt_loopback(2))),
                format!("{base}/nodes/announce"),
                announce_body(&info, &fabricated_url(total + 1)),
            )
            .await;
            r.check(
                other.status == 200,
                "another source address has its own budget",
                format!("status {}", other.status),
            );
        }
        "cap" => {
            let cap = ctx.num("cap", 50) as usize;
            let total = cap * 4;
            let make: RequestFn = {
                let (c, info, base) = (c.clone(), info.clone(), base.clone());
                Arc::new(move |i| {
                    (
                        "POST",
                        c.post(format!("{base}/nodes/announce"))
                            .json(&announce_body(&info, &fabricated_url(i))),
                    )
                })
            };
            let (samples, elapsed) = run_load(8, Stop::Count(total), NO_THINK, make).await;
            let t = tally(&samples);
            r.metric("cap", cap);
            r.metric("distinct_new_urls_sent", total);
            r.metric("statuses", t.render());
            r.metric(
                "announce_per_sec",
                round(total as f64 / elapsed.as_secs_f64()),
            );
            let urls = peer_urls(ctx, &base).await?;
            r.metric("peer_table_size", urls.len());
            r.check(
                urls.len() <= cap,
                "the peer table never exceeds its cap",
                format!("{} <= {cap}", urls.len()),
            );
            r.check(
                urls.len() == cap,
                "the table filled to the cap by evicting oldest entries",
                format!("{}", urls.len()),
            );
            r.check(
                t.server_errors() == 0,
                "no 5xx or transport errors",
                t.render(),
            );
            r.check(
                urls.contains(&fabricated_url(total - 1)),
                "the newest announcer is present",
                "",
            );
            r.check(
                !urls.contains(&fabricated_url(0)),
                "the oldest announcer was evicted",
                "",
            );
        }
        "strict" => {
            let cases: Vec<(String, u16, &str)> = vec![
                ("http://10.1.2.3:8080".into(), 403, "private range"),
                ("http://192.168.1.10:8080".into(), 403, "private range"),
                ("http://127.0.0.1:9".into(), 403, "loopback"),
                (
                    "http://169.254.169.254".into(),
                    403,
                    "cloud metadata address",
                ),
                ("http://user:pw@example.invalid".into(), 400, "userinfo"),
                ("ftp://example.invalid".into(), 400, "scheme"),
                (
                    format!("http://{}.example.invalid", "a".repeat(300)),
                    400,
                    "over-long",
                ),
            ];
            let mut all_ok = true;
            for (url, want, why) in &cases {
                let s = post(
                    c.clone(),
                    format!("{base}/nodes/announce"),
                    announce_body(&info, url),
                )
                .await;
                let ok = s.status == *want;
                all_ok &= ok;
                r.check(
                    ok,
                    &format!("announce of a {why} address is refused"),
                    format!("status {} code {:?}, want {want}", s.status, s.code),
                );
            }
            let peers = peer_urls(ctx, &base).await?.len();
            r.metric("peer_table_size", peers);
            r.check(
                all_ok && peers == 0,
                "no refused address entered the peer table",
                format!("{peers}"),
            );
        }
        other => anyhow::bail!("unknown announce-flood mode {other}"),
    }
    Ok(r)
}

/// `--target` is node A of `--nodes a,b,c`; A is flooded with fabricated announces.
pub async fn mesh_announce_flood(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("mesh-announce-flood");
    let nodes: Vec<String> = ctx.args["nodes"]
        .split(',')
        .map(|s| s.trim_end_matches('/').to_string())
        .collect();
    let cap = ctx.num("cap", 60) as usize;
    let interval = ctx.num("announce-interval", 2);
    let started = Instant::now();
    let mut converged = false;
    while started.elapsed() < Duration::from_secs(30) {
        let mut all = true;
        for n in &nodes {
            let peers = peer_urls(ctx, n).await.unwrap_or_default();
            all &= nodes.iter().filter(|o| *o != n).all(|o| peers.contains(o));
        }
        if all {
            converged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    r.metric("convergence_secs", round(started.elapsed().as_secs_f64()));
    let mut before = Vec::new();
    for n in &nodes {
        before.push(json!({"node": n, "peers": peer_urls(ctx, n).await.unwrap_or_default()}));
    }
    r.metric("tables_before_flood", Value::Array(before));
    r.check(
        converged,
        "every node learned every other node before the flood",
        "",
    );

    let info = ctx.node_info().await?;
    let a = nodes[0].clone();
    let c = ctx.client(None);
    let total = cap * 5;
    let make: RequestFn = {
        let (c, info, a) = (c.clone(), info.clone(), a.clone());
        Arc::new(move |i| {
            (
                "POST",
                c.post(format!("{a}/nodes/announce"))
                    .json(&announce_body(&info, &fabricated_url(i))),
            )
        })
    };
    let (samples, _) = run_load(8, Stop::Count(total), NO_THINK, make).await;
    let t = tally(&samples);
    r.metric("flood_statuses", t.render());
    r.check(
        t.server_errors() == 0,
        "no 5xx or transport errors on the flooded node",
        t.render(),
    );
    tokio::time::sleep(Duration::from_secs(interval * 4)).await;

    let mut sizes = Vec::new();
    for n in &nodes {
        let peers = peer_urls(ctx, n).await?;
        let fabricated = peers.iter().filter(|p| p.starts_with("http://10.")).count();
        r.check(
            peers.len() <= cap,
            &format!("{n} stays within its peer table cap"),
            format!("{} <= {cap}", peers.len()),
        );
        sizes.push(json!({"node": n, "peers": peers.len(), "fabricated": fabricated}));
    }
    r.metric("tables_after_settle", Value::Array(sizes));
    let a_peers = peer_urls(ctx, &a).await?;
    r.check(
        nodes[1..].iter().all(|o| a_peers.contains(o)),
        "the flooded node still lists its real peers after settling",
        "",
    );
    let b_status = ctx
        .client(None)
        .get(format!("{}/nodes/status", nodes[1]))
        .send()
        .await?;
    r.check(
        b_status.status().is_success(),
        "the other nodes keep serving",
        format!("{}", b_status.status()),
    );
    Ok(r)
}

async fn node_resources(ctx: &Ctx) -> Value {
    ctx.node_info()
        .await
        .map(|v| v["resources"].clone())
        .unwrap_or(Value::Null)
}

pub async fn concurrency(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("concurrency");
    let cap = ctx.num("cap", 4);
    let secs = ctx.scaled(4, 20);
    let workers = ctx.scaled(32, 64) as usize;
    let ids = seed_identities(ctx.pool()?, 9).await?;
    let probe_token = ids[0].1.clone();
    let load_tokens: Arc<Vec<String>> = Arc::new(ids[1..].iter().map(|(_, t)| t.clone()).collect());
    let c = ctx.client(None);
    let url = ctx.url("/me");

    let probe = |c: reqwest::Client, url: String, token: String, n: usize, gap: Duration| async move {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(send_one("probe", c.get(&url).bearer_auth(&token)).await);
            tokio::time::sleep(gap).await;
        }
        out
    };
    let baseline = probe(
        c.clone(),
        url.clone(),
        probe_token.clone(),
        30,
        Duration::from_millis(20),
    )
    .await;
    let base_lat = latency(&baseline);

    let sampler = ctx.pid.map(Sampler::start);
    let make: RequestFn = {
        let (c, url, tokens) = (c.clone(), url.clone(), load_tokens.clone());
        Arc::new(move |i| ("load", c.get(&url).bearer_auth(&tokens[i % tokens.len()])))
    };
    let load = tokio::spawn(run_load(
        workers,
        Stop::For(Duration::from_secs(secs)),
        NO_THINK,
        make,
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let probes = probe(
        c.clone(),
        url.clone(),
        probe_token,
        (secs * 15) as usize,
        Duration::from_millis(50),
    )
    .await;
    let (load_samples, elapsed) = load.await?;
    let usage = match sampler {
        Some(s) => Some(s.finish().await),
        None => None,
    };
    let lt = tally(&load_samples);
    let pt = tally(&probes);
    let (ll, pl) = (latency(&load_samples), latency(&probes));
    r.metric("max_concurrent_requests", cap);
    r.metric("load_workers", workers);
    r.metric(
        "load_rps",
        round(load_samples.len() as f64 / elapsed.as_secs_f64()),
    );
    r.metric("load_statuses", lt.render());
    r.metric("load_latency", latency_json(&ll));
    r.metric("probe_baseline_latency", latency_json(&base_lat));
    r.metric("probe_under_load_latency", latency_json(&pl));
    if base_lat.p50_ms > 0.0 {
        r.metric("probe_p50_slowdown_x", round(pl.p50_ms / base_lat.p50_ms));
    }
    if let Some(u) = usage {
        r.metric("server_peak_rss_mb", round(u.peak_rss_mb));
        r.metric("server_avg_cpu_pct", round(u.avg_cpu_pct));
    }
    r.check(
        lt.server_errors() == 0 && lt.count(429) == 0,
        "saturating the cap backpressures: no drops, 5xx or 429",
        lt.render(),
    );
    r.check(
        pt.ok() == pt.total(),
        "a well-behaved client is served (slowly) rather than refused",
        pt.render(),
    );
    Ok(r)
}

pub async fn db_pool(ctx: &Ctx) -> Result<Report> {
    let mix = ctx
        .args
        .get("mix")
        .cloned()
        .unwrap_or_else(|| "mixed".into());
    let mut r = Report::new(&format!("db-pool-{mix}"));
    let pool_size = ctx.num("pool", 2);
    let secs = ctx.scaled(4, 20);
    let workers = ctx.scaled(32, 96) as usize;
    let ids = seed_identities(ctx.pool()?, 50).await?;
    let tokens: Arc<Vec<String>> = Arc::new(ids.into_iter().map(|(_, t)| t).collect());
    let c = ctx.client(None);
    let base = ctx.url("");
    let make: RequestFn = {
        let (c, tokens, mix) = (c.clone(), tokens.clone(), mix.clone());
        Arc::new(move |i| {
            let tok = &tokens[i % tokens.len()];
            let slot = match mix.as_str() {
                "reads" => 5 + (i * 7919) % 5,
                "writes" => 0,
                _ => (i * 7919) % 10,
            };
            match slot {
                0..=4 => (
                    "PATCH /me",
                    c.patch(format!("{base}/me"))
                        .bearer_auth(tok)
                        .json(&json!({"bio": format!("load {i}")})),
                ),
                5..=7 => (
                    "GET /me/history",
                    c.get(format!("{base}/me/history")).bearer_auth(tok),
                ),
                _ => (
                    "GET /identities/search",
                    c.get(format!("{base}/identities/search?q=load"))
                        .bearer_auth(tok),
                ),
            }
        })
    };
    let sampler = ctx.pid.map(Sampler::start);
    let mid = {
        let ctx_url = ctx.url("/nodes/status");
        let c = ctx.client(None);
        let wait = Duration::from_secs(secs / 2);
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            c.get(ctx_url)
                .send()
                .await
                .ok()?
                .json::<Value>()
                .await
                .ok()
                .map(|v| v["resources"].clone())
        })
    };
    let (samples, elapsed) = run_load(
        workers,
        Stop::For(Duration::from_secs(secs)),
        NO_THINK,
        make,
    )
    .await;
    let usage = match sampler {
        Some(s) => Some(s.finish().await),
        None => None,
    };
    let t = tally(&samples);
    r.metric("mix", mix);
    r.metric("max_db_connections", pool_size);
    r.metric("workers", workers);
    r.metric("rps", round(samples.len() as f64 / elapsed.as_secs_f64()));
    r.metric("statuses", t.render());
    r.metric("latency", latency_json(&latency(&samples)));
    for label in ["PATCH /me", "GET /me/history", "GET /identities/search"] {
        let by: Vec<_> = samples
            .iter()
            .filter(|s| s.label == label)
            .cloned()
            .collect();
        r.metric(&format!("latency {label}"), latency_json(&latency(&by)));
        r.metric(&format!("statuses {label}"), tally(&by).render());
    }
    r.metric(
        "resources_mid_load",
        mid.await?.unwrap_or(Value::Null)["db_pool"].clone(),
    );
    r.metric(
        "resources_after",
        node_resources(ctx).await["db_pool"].clone(),
    );
    if let Some(u) = usage {
        r.metric("server_peak_rss_mb", round(u.peak_rss_mb));
        r.metric("server_avg_cpu_pct", round(u.avg_cpu_pct));
    }
    r.check(
        t.count(0) == 0,
        "no transport errors or timeouts under pool exhaustion",
        t.render(),
    );
    let healthy = ctx
        .client(None)
        .get(ctx.url("/nodes/status"))
        .send()
        .await?;
    r.check(
        healthy.status().is_success(),
        "the node serves normally afterwards",
        format!("{}", healthy.status()),
    );
    Ok(r)
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Idle,
    Headers,
    Body,
}

#[derive(Default)]
struct KindStats {
    connect_failed: u64,
    open: u64,
    closed: u64,
    close_secs: Vec<f64>,
    statuses: Vec<u16>,
}

enum Outcome {
    ConnectFailed,
    ClosedAt(Duration, Option<u16>),
    OpenAtEnd,
}

async fn slow_conn_task(addr: String, kind: Kind, observe: Duration) -> Outcome {
    let Ok(mut s) = TcpStream::connect(&addr).await else {
        return Outcome::ConnectFailed;
    };
    let start_bytes: &[u8] = match kind {
        Kind::Idle => b"",
        Kind::Headers => b"GET /nodes/status HTTP/1.1\r\nHost: localhost\r\nX-Slow: a",
        Kind::Body => b"POST /nodes/announce HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100000\r\n\r\n{",
    };
    if !start_bytes.is_empty() && s.write_all(start_bytes).await.is_err() {
        return Outcome::ClosedAt(Duration::ZERO, None);
    }
    let started = Instant::now();
    let mut buf = [0u8; 512];
    let mut status = None;
    loop {
        if started.elapsed() >= observe {
            return Outcome::OpenAtEnd;
        }
        tokio::select! {
            n = s.read(&mut buf) => match n {
                Ok(0) | Err(_) => return Outcome::ClosedAt(started.elapsed(), status),
                Ok(n) => {
                    if status.is_none() {
                        status = std::str::from_utf8(&buf[..n]).ok().and_then(|t| t.split_whitespace().nth(1)).and_then(|c| c.parse().ok());
                    }
                }
            },
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                let byte: &[u8] = match kind { Kind::Idle => b"", Kind::Headers => b"a", Kind::Body => b" " };
                if !byte.is_empty() && s.write_all(byte).await.is_err() {
                    return Outcome::ClosedAt(started.elapsed(), status);
                }
            }
        }
    }
}

pub async fn slow_conn(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("slow-conn");
    let conns = ctx.num("conns", ctx.scaled(30, 150)) as usize;
    let observe = Duration::from_secs(ctx.num("observe", ctx.scaled(4, 75)));
    let addr = ctx.host_port();
    r.metric(
        "open_file_count_baseline",
        node_resources(ctx).await["open_file_count"].clone(),
    );
    let mut tasks = Vec::new();
    for (name, kind) in [
        ("idle", Kind::Idle),
        ("slow-headers", Kind::Headers),
        ("slow-body", Kind::Body),
    ] {
        for _ in 0..conns {
            tasks.push((
                name,
                tokio::spawn(slow_conn_task(addr.clone(), kind, observe)),
            ));
        }
    }
    let probe_c = ctx.client(None);
    let probe_url = ctx.url("/nodes/status");
    let probe = tokio::spawn(async move {
        let mut out = Vec::new();
        let end = Instant::now() + observe;
        while Instant::now() < end {
            out.push(send_one("probe", probe_c.get(&probe_url)).await);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        out
    });
    tokio::time::sleep(Duration::from_secs(2).min(observe)).await;
    let resources_peak = node_resources(ctx).await;
    r.metric(
        "open_file_count_with_slow_connections",
        resources_peak["open_file_count"].clone(),
    );
    let mut per: std::collections::BTreeMap<&str, KindStats> = Default::default();
    for (name, h) in tasks {
        let e = per.entry(name).or_default();
        match h.await? {
            Outcome::ConnectFailed => e.connect_failed += 1,
            Outcome::OpenAtEnd => e.open += 1,
            Outcome::ClosedAt(d, st) => {
                e.closed += 1;
                e.close_secs.push(d.as_secs_f64());
                e.statuses.extend(st);
            }
        }
    }
    r.metric("connections_per_kind", conns);
    r.metric("observation_secs", observe.as_secs());
    for (name, k) in &per {
        let (lo, hi) = k
            .close_secs
            .iter()
            .fold((f64::MAX, 0f64), |(l, h), t| (l.min(*t), h.max(*t)));
        let closes = !k.close_secs.is_empty();
        r.metric(
            name,
            json!({
                "connect_failed": k.connect_failed, "open_at_end": k.open, "closed_by_server": k.closed,
                "first_close_secs": if closes { json!(round(lo)) } else { Value::Null },
                "last_close_secs": if closes { json!(round(hi)) } else { Value::Null },
                "responses_before_close": k.statuses,
            }),
        );
    }
    let probes = probe.await?;
    let pt = tally(&probes);
    r.metric("probe_statuses", pt.render());
    r.metric("probe_latency", latency_json(&latency(&probes)));
    r.check(
        pt.ok() == pt.total(),
        "well-behaved requests keep being served while slow connections are held open",
        pt.render(),
    );
    let healthy = ctx
        .client(None)
        .get(ctx.url("/nodes/status"))
        .send()
        .await?;
    r.check(
        healthy.status().is_success(),
        "the node serves normally afterwards",
        format!("{}", healthy.status()),
    );
    Ok(r)
}

/// Sends raw bytes (headers, `body_len` filler bytes, then `tail`) and returns the response
/// status, or 0 when the server closed without answering.
async fn raw_request(addr: &str, head: String, body_len: usize, tail: &[u8]) -> u16 {
    let Ok(mut s) = TcpStream::connect(addr).await else {
        return 0;
    };
    if s.write_all(head.as_bytes()).await.is_err() {
        return read_status(&mut s).await;
    }
    let chunk = vec![b'a'; 64 * 1024];
    let mut left = body_len;
    while left > 0 {
        let n = left.min(chunk.len());
        if s.write_all(&chunk[..n]).await.is_err() {
            return read_status(&mut s).await;
        }
        left -= n;
    }
    let _ = s.write_all(tail).await;
    read_status(&mut s).await
}

async fn read_status(s: &mut TcpStream) -> u16 {
    let mut buf = [0u8; 512];
    match tokio::time::timeout(Duration::from_secs(30), s.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => std::str::from_utf8(&buf[..n])
            .ok()
            .and_then(|t| t.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0),
        _ => 0,
    }
}

pub async fn oversized(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("oversized");
    let addr = ctx.host_port();
    let token = seed_identities(ctx.pool()?, 1).await?.remove(0).1;
    let sampler = ctx.pid.map(Sampler::start);
    let kib = 1024usize;
    let max_body = ctx.scaled(8, 64) as usize * 1024 * kib;
    let mut sizes = vec![
        kib,
        64 * kib,
        1024 * kib,
        2048 * kib - kib,
        2048 * kib + kib,
        8 * 1024 * kib,
        max_body,
    ];
    sizes.dedup();
    let mut body_results = Vec::new();
    for (label, method, path, auth) in [
        (
            "POST /nodes/announce (public)",
            "POST",
            "/nodes/announce",
            false,
        ),
        ("PATCH /me (authenticated)", "PATCH", "/me", true),
    ] {
        for size in &sizes {
            let overhead = 10;
            let filler = size.saturating_sub(overhead);
            let head = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{{\"pad\":\"",
                if auth { format!("Authorization: Bearer {token}\r\n") } else { String::new() },
                filler + overhead,
            );
            // Body is `{"pad":"` + filler + `"}`; the filler is sent raw and the two closing bytes follow it.
            let status = raw_request(&addr, head, filler, b"\"}").await;
            r.metric(&format!("{label} body {} KiB", size / kib), status);
            body_results.push((label, *size, status));
        }
    }
    let mut header_results = Vec::new();
    for size in [8 * kib, 32 * kib, 64 * kib, 1024 * kib] {
        let head = format!(
            "GET /nodes/status HTTP/1.1\r\nHost: localhost\r\nX-Pad: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(size)
        );
        let status = raw_request(&addr, head, 0, b"").await;
        r.metric(&format!("request header value {} KiB", size / kib), status);
        header_results.push((size, status));
    }
    for size in [16 * kib, 64 * kib, 1024 * kib] {
        let head = format!(
            "GET /nodes/status?q={} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            "a".repeat(size)
        );
        let status = raw_request(&addr, head, 0, b"").await;
        r.metric(&format!("request target {} KiB", size / kib), status);
    }
    let many = (0..400)
        .map(|i| format!("X-H{i}: v\r\n"))
        .collect::<String>();
    let status = raw_request(
        &addr,
        format!("GET /nodes/status HTTP/1.1\r\nHost: localhost\r\n{many}Connection: close\r\n\r\n"),
        0,
        b"",
    )
    .await;
    r.metric("400 request headers", status);
    let usage = match sampler {
        Some(s) => Some(s.finish().await),
        None => None,
    };
    if let Some(u) = &usage {
        r.metric("server_peak_rss_mb", round(u.peak_rss_mb));
        r.check(
            u.peak_rss_mb < 1024.0,
            "server memory stays bounded across oversized requests",
            format!("{:.0} MB", u.peak_rss_mb),
        );
    }
    let healthy = ctx
        .client(None)
        .get(ctx.url("/nodes/status"))
        .send()
        .await?;
    r.check(
        healthy.status().is_success(),
        "the node serves normally afterwards",
        format!("{}", healthy.status()),
    );
    let small_ok = body_results
        .iter()
        .filter(|(_, s, _)| *s <= 1024 * kib)
        .all(|(_, _, st)| *st != 413 && *st != 0);
    r.check(small_ok, "bodies up to 1 MiB reach the handler", "");
    let big_rejected = body_results
        .iter()
        .filter(|(_, s, _)| *s >= 8 * 1024 * kib)
        .all(|(_, _, st)| *st == 413 || *st == 0);
    r.check(big_rejected, "bodies of 8 MiB and larger are refused", "");
    Ok(r)
}

pub async fn sustained(ctx: &Ctx) -> Result<Report> {
    let mut r = Report::new("sustained");
    let secs = ctx.scaled(5, 60);
    let workers = ctx.num("workers", ctx.scaled(8, 64)) as usize;
    let think = Duration::from_millis(ctx.num("think-ms", 20));
    let ids = seed_identities(ctx.pool()?, ctx.scaled(20, 200) as usize).await?;
    let tokens: Arc<Vec<String>> = Arc::new(ids.into_iter().map(|(_, t)| t).collect());
    let c = ctx.client(None);
    let base = ctx.url("");
    let make: RequestFn = {
        let (c, tokens) = (c.clone(), tokens.clone());
        Arc::new(move |i| {
            let tok = &tokens[i % tokens.len()];
            match (i * 7919) % 100 {
                0..=39 => ("GET /me", c.get(format!("{base}/me")).bearer_auth(tok)),
                40..=54 => ("GET /nodes/status", c.get(format!("{base}/nodes/status"))),
                55..=69 => (
                    "GET /me/history",
                    c.get(format!("{base}/me/history")).bearer_auth(tok),
                ),
                70..=79 => (
                    "GET /identities/search",
                    c.get(format!("{base}/identities/search?q=load"))
                        .bearer_auth(tok),
                ),
                80..=89 => (
                    "PUT /me/presence",
                    c.put(format!("{base}/me/presence"))
                        .bearer_auth(tok)
                        .json(&json!({"status": "Online"})),
                ),
                _ => (
                    "PATCH /me",
                    c.patch(format!("{base}/me"))
                        .bearer_auth(tok)
                        .json(&json!({"bio": format!("load {i}")})),
                ),
            }
        })
    };
    let sampler = ctx.pid.map(Sampler::start);
    let (samples, elapsed) =
        run_load(workers, Stop::For(Duration::from_secs(secs)), think, make).await;
    let usage = match sampler {
        Some(s) => Some(s.finish().await),
        None => None,
    };
    let t = tally(&samples);
    r.metric("workers", workers);
    r.metric("think_ms", think.as_millis() as u64);
    r.metric("duration_secs", round(elapsed.as_secs_f64()));
    r.metric("requests", samples.len());
    r.metric(
        "throughput_rps",
        round(samples.len() as f64 / elapsed.as_secs_f64()),
    );
    r.metric("statuses", t.render());
    r.metric(
        "error_rate_pct",
        round((t.total() - t.ok()) as f64 * 100.0 / t.total().max(1) as f64),
    );
    r.metric("latency", latency_json(&latency(&samples)));
    for label in [
        "GET /me",
        "GET /nodes/status",
        "GET /me/history",
        "GET /identities/search",
        "PUT /me/presence",
        "PATCH /me",
    ] {
        let by: Vec<_> = samples
            .iter()
            .filter(|s| s.label == label)
            .cloned()
            .collect();
        r.metric(&format!("latency {label}"), latency_json(&latency(&by)));
        r.metric(&format!("statuses {label}"), tally(&by).render());
    }
    if let Some(u) = usage {
        r.metric("server_peak_rss_mb", round(u.peak_rss_mb));
        r.metric("server_avg_rss_mb", round(u.avg_rss_mb));
        r.metric("server_avg_cpu_pct_of_one_core", round(u.avg_cpu_pct));
        r.metric("server_peak_cpu_pct_of_one_core", round(u.peak_cpu_pct));
    }
    r.check(
        t.total() - t.ok() == 0,
        "no errors and no limiting at a realistic profile",
        t.render(),
    );
    Ok(r)
}
