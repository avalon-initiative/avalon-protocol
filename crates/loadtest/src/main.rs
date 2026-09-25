//! `avalon-loadtest <scenario> --target <url> [--key value ...]`
//!
//! Drives one scenario against a node the caller started (see
//! `scripts/load-tests.sh`), prints `CHECK`/`METRIC` lines and a final
//! `RESULT {json}` line, and exits non-zero when any check failed.

mod common;
mod scenarios;

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use avalon_loadtest::target;

use common::Ctx;

fn parse_args() -> Result<(String, HashMap<String, String>)> {
    let mut it = std::env::args().skip(1);
    let scenario = it
        .next()
        .context("usage: avalon-loadtest <scenario> --target <url> ...")?;
    let mut args = HashMap::new();
    while let Some(k) = it.next() {
        let key = k
            .strip_prefix("--")
            .with_context(|| format!("unexpected argument {k}"))?;
        let v = it
            .next()
            .with_context(|| format!("--{key} needs a value"))?;
        args.insert(key.to_string(), v);
    }
    Ok((scenario, args))
}

#[tokio::main]
async fn main() -> Result<()> {
    let (scenario, args) = parse_args()?;
    let raw_target = args.get("target").context("--target is required")?;
    let base = target::check_target(raw_target, target::override_enabled())?;
    if let Some(extra) = args.get("nodes") {
        for n in extra.split(',') {
            target::check_target(n, target::override_enabled())?;
        }
    }
    let ctx = Ctx::new(base, args).await?;
    let report = match scenario.as_str() {
        "flood-public" => scenarios::flood_public(&ctx).await?,
        "flood-auth" => scenarios::flood_auth(&ctx).await?,
        "many-identities" => scenarios::many_identities(&ctx).await?,
        "announce-flood" => scenarios::announce_flood(&ctx).await?,
        "mesh-announce-flood" => scenarios::mesh_announce_flood(&ctx).await?,
        "concurrency" => scenarios::concurrency(&ctx).await?,
        "db-pool" => scenarios::db_pool(&ctx).await?,
        "slow-conn" => scenarios::slow_conn(&ctx).await?,
        "oversized" => scenarios::oversized(&ctx).await?,
        "sustained" => scenarios::sustained(&ctx).await?,
        other => bail!("unknown scenario {other}"),
    };
    report.print();
    if report.failed() {
        std::process::exit(1);
    }
    Ok(())
}
