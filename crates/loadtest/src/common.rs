//! Shared scenario plumbing: context, request runner, seeding, reporting.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use avalon_loadtest::stats::{summarize, LatencySummary, StatusTally};
use reqwest::RequestBuilder;
use serde_json::{json, Map, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

pub struct Ctx {
    pub base: Url,
    pub args: HashMap<String, String>,
    pub db: Option<PgPool>,
    pub pid: Option<u32>,
    pub smoke: bool,
}

impl Ctx {
    pub async fn new(base: Url, args: HashMap<String, String>) -> Result<Self> {
        let db = match args.get("db-url") {
            Some(url) => Some(
                PgPoolOptions::new()
                    .max_connections(4)
                    .connect(url)
                    .await
                    .context("connecting to the scenario database")?,
            ),
            None => None,
        };
        let pid = args.get("pid").and_then(|p| p.parse().ok());
        let smoke = args.get("scale").is_none_or(|s| s != "full");
        Ok(Self {
            base,
            args,
            db,
            pid,
            smoke,
        })
    }

    pub fn num(&self, key: &str, default: u64) -> u64 {
        self.args
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// `smoke` at smoke scale, `full` otherwise.
    pub fn scaled(&self, smoke: u64, full: u64) -> u64 {
        if self.smoke {
            smoke
        } else {
            full
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.as_str().trim_end_matches('/'), path)
    }

    pub fn host_port(&self) -> String {
        format!(
            "{}:{}",
            self.base.host_str().unwrap_or("127.0.0.1"),
            self.base.port_or_known_default().unwrap_or(80)
        )
    }

    /// A client bound to `local` as its source address when given.
    pub fn client(&self, local: Option<IpAddr>) -> reqwest::Client {
        let mut b = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(256);
        if let Some(ip) = local {
            b = b.local_address(ip);
        }
        b.build().expect("client")
    }

    pub fn pool(&self) -> Result<&PgPool> {
        self.db
            .as_ref()
            .context("--db-url is required for this scenario")
    }

    pub async fn node_info(&self) -> Result<Value> {
        Ok(self
            .client(None)
            .get(self.url("/nodes/status"))
            .send()
            .await?
            .json()
            .await?)
    }
}

/// Seeds `n` identities with sessions directly in the node's schema.
pub async fn seed_identities(pool: &PgPool, n: usize) -> Result<Vec<(Uuid, String)>> {
    let ids: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
    let tokens: Vec<String> = ids
        .iter()
        .map(|_| format!("load-{}", Uuid::new_v4()))
        .collect();
    let names: Vec<String> = ids.iter().map(|id| format!("load-{id}")).collect();
    let expires = time::OffsetDateTime::now_utc() + time::Duration::hours(2);
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO identities (id) SELECT unnest($1::uuid[])")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO profiles (identity_id, display_name) SELECT unnest($1::uuid[]), unnest($2::text[])")
        .bind(&ids)
        .bind(&names)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) SELECT unnest($1::text[]), unnest($2::uuid[]), $3")
        .bind(&tokens)
        .bind(&ids)
        .bind(expires)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ids.into_iter().zip(tokens).collect())
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub label: &'static str,
    /// 0 for a transport error or timeout.
    pub status: u16,
    pub latency: Duration,
    pub retry_after: bool,
    pub code: Option<String>,
}

pub type RequestFn = Arc<dyn Fn(usize) -> (&'static str, RequestBuilder) + Send + Sync>;

pub enum Stop {
    Count(usize),
    For(Duration),
}

pub async fn send_one(label: &'static str, rb: RequestBuilder) -> Sample {
    let started = Instant::now();
    match rb.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let retry_after = resp.headers().contains_key("retry-after");
            let body = resp.text().await.unwrap_or_default();
            let code = if status >= 400 {
                serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("code").and_then(|c| c.as_str().map(String::from)))
            } else {
                None
            };
            Sample {
                label,
                status,
                latency: started.elapsed(),
                retry_after,
                code,
            }
        }
        Err(_) => Sample {
            label,
            status: 0,
            latency: started.elapsed(),
            retry_after: false,
            code: None,
        },
    }
}

/// Runs `make` from `workers` concurrent closed-loop workers until `stop`.
pub async fn run_load(
    workers: usize,
    stop: Stop,
    think: Duration,
    make: RequestFn,
) -> (Vec<Sample>, Duration) {
    let next = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let deadline = match stop {
        Stop::For(d) => Some(started + d),
        Stop::Count(_) => None,
    };
    let limit = match stop {
        Stop::Count(n) => n,
        Stop::For(_) => usize::MAX,
    };
    let mut handles = Vec::new();
    for _ in 0..workers {
        let (next, make) = (next.clone(), make.clone());
        handles.push(tokio::spawn(async move {
            let mut out = Vec::new();
            loop {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= limit {
                    break;
                }
                let (label, rb) = make(i);
                out.push(send_one(label, rb).await);
                if !think.is_zero() {
                    tokio::time::sleep(think).await;
                }
            }
            out
        }));
    }
    let mut all = Vec::new();
    for h in handles {
        all.extend(h.await.unwrap_or_default());
    }
    (all, started.elapsed())
}

pub fn tally(samples: &[Sample]) -> StatusTally {
    let mut t = StatusTally::default();
    for s in samples {
        t.record(s.status);
    }
    t
}

pub fn latency(samples: &[Sample]) -> LatencySummary {
    let d: Vec<_> = samples.iter().map(|s| s.latency).collect();
    summarize(&d)
}

pub fn latency_json(l: &LatencySummary) -> Value {
    json!({"count": l.count, "p50_ms": round(l.p50_ms), "p95_ms": round(l.p95_ms), "p99_ms": round(l.p99_ms), "max_ms": round(l.max_ms)})
}

pub fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub struct Report {
    scenario: String,
    checks: Vec<(bool, String, String)>,
    metrics: Map<String, Value>,
}

impl Report {
    pub fn new(scenario: &str) -> Self {
        Self {
            scenario: scenario.to_string(),
            checks: Vec::new(),
            metrics: Map::new(),
        }
    }

    pub fn check(&mut self, ok: bool, desc: &str, detail: impl Into<String>) {
        self.checks.push((ok, desc.to_string(), detail.into()));
    }

    pub fn metric(&mut self, key: &str, value: impl Into<Value>) {
        self.metrics.insert(key.to_string(), value.into());
    }

    pub fn failed(&self) -> bool {
        self.checks.iter().any(|(ok, _, _)| !ok)
    }

    pub fn print(&self) {
        println!("== scenario: {}", self.scenario);
        for (k, v) in &self.metrics {
            println!("METRIC {k} = {v}");
        }
        for (ok, desc, detail) in &self.checks {
            println!(
                "CHECK {} {desc} ({detail})",
                if *ok { "PASS" } else { "FAIL" }
            );
        }
        println!(
            "RESULT {}",
            json!({"scenario": self.scenario, "passed": !self.failed(), "metrics": self.metrics})
        );
    }
}
