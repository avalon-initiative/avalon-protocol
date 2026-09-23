//! `avalon` — local dev/ops CLI.
//!
//! Always available, in any build: `avalon inspect-ledger`,
//! `avalon inspect-ledger-full` (same view, plus each entry's payload),
//! `avalon outbox-status` — read-only diagnostics, safe against any
//! deployment including a real one. `avalon logs export`
//! reads `avalon-server`'s own log file, strips ANSI codes, redacts
//! known-sensitive values, and normalizes it to line-delimited JSON so a
//! hoster can safely attach it to a filed GitHub issue — see
//! `logs_export.rs`'s own module doc comment. `avalon prune-ledger` is
//! the operator-facing entry point for node-tiered retention pruning — see
//! `avalon_chain::retention`'s module doc comment for the full design;
//! this command itself only reads `AVALON_RETENTION_*` from the
//! environment and reports/executes exactly what that config says, nothing
//! more.
//!
//! Available only when this binary is built with the default `dev-tools`
//! Cargo feature (see `dev_tools.rs`'s own doc comment for the
//! full rationale): `avalon create-identity`, `avalon login <identity_id>`,
//! `avalon register-integrator` (renamed from
//! `register-game` — the old name stays a working alias), and
//! `avalon pair-device`
//! (drives the `start`/`poll` side of cross-device pairing,
//! standing in for a real WebAuthn-incapable client so that flow is
//! testable without a real console/engine), `avalon issue-achievement`
//! (issues an already-defined achievement to the identity
//! behind `--token`, through `avalon-sdk`, resolving the issuer's
//! signing key/key id from what `register-integrator` saved unless
//! overridden), and `avalon register-issuer` (registers an integrator's key as an issuer on an
//! explicitly declared target network, `--network-id <id>` or
//! `--env <dev|int|mainnet>`, refusing client-side on a mismatch against
//! the server's independently STH-verified network rather than trusting
//! its bare `network_id`). A build compiled with `--no-default-features` doesn't
//! merely refuse these commands at runtime — they, and every dependency
//! only they need, are absent from the binary entirely.

#[cfg(feature = "dev-tools")]
mod dev_tools;
mod logs_export;

use avalon_chain::PostgresSettlementProvider;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    avalon_devenv::load();

    let mut args = std::env::args();
    let command = args.nth(1);
    match command.as_deref() {
        Some("inspect-ledger") => inspect_ledger(false).await,
        Some("inspect-ledger-full") => inspect_ledger(true).await,
        #[cfg(feature = "dev-tools")]
        Some("create-identity") => dev_tools::create_identity().await,
        #[cfg(feature = "dev-tools")]
        Some("login") => {
            let Some(identity_id) = args.next().and_then(|s| s.parse::<Uuid>().ok()) else {
                eprintln!("usage: avalon login <identity_id>");
                std::process::exit(1);
            };
            dev_tools::login(identity_id).await;
        }
        #[cfg(feature = "dev-tools")]
        Some("pair-device") => dev_tools::pair_device().await,
        Some("outbox-status") => outbox_status().await,
        Some("prune-ledger") => {
            let dry_run = args.any(|a| a == "--dry-run");
            prune_ledger(dry_run).await;
        }
        Some("rebuild-index") => rebuild_index().await,
        Some("migrate-network") => {
            let raw_args: Vec<String> = args.collect();
            migrate_network(&raw_args).await;
        }
        Some("discover-mirror-peers") => discover_mirror_peers().await,
        Some("check-switch-readiness") => {
            let raw_args: Vec<String> = args.collect();
            check_switch_readiness(&raw_args).await;
        }
        Some("list-equivocations") => {
            list_equivocations(args.next()).await;
        }
        Some("verify-mirror-convergence") => {
            let raw_args: Vec<String> = args.collect();
            verify_mirror_convergence(&raw_args).await;
        }
        Some("logs") => {
            let sub = args.next();
            match sub.as_deref() {
                Some("export") => {
                    let raw_args: Vec<String> = args.collect();
                    match logs_export::ExportArgs::parse(&raw_args) {
                        Ok(parsed) => logs_export::run(parsed),
                        Err(message) => {
                            eprintln!("{message}");
                            eprintln!("{}", logs_export::USAGE);
                            std::process::exit(1);
                        }
                    }
                }
                _ => {
                    eprintln!("{}", logs_export::USAGE);
                    std::process::exit(1);
                }
            }
        }
        Some("resolve-equivocation") => {
            let raw_args: Vec<String> = args.collect();
            resolve_equivocation(&raw_args).await;
        }
        #[cfg(feature = "dev-tools")]
        Some(cmd) if is_register_integrator_command(cmd) => {
            let raw_args: Vec<String> = args.collect();
            match dev_tools::RegisterIntegratorArgs::parse(&raw_args) {
                Ok(parsed) => dev_tools::register_integrator(parsed).await,
                Err(message) => {
                    eprintln!("{message}");
                    eprintln!("{}", dev_tools::REGISTER_INTEGRATOR_USAGE);
                    std::process::exit(1);
                }
            }
        }
        #[cfg(feature = "dev-tools")]
        Some("issue-achievement") => {
            let raw_args: Vec<String> = args.collect();
            match dev_tools::IssueAchievementArgs::parse(&raw_args) {
                Ok(parsed) => dev_tools::issue_achievement(parsed).await,
                Err(message) => {
                    eprintln!("{message}");
                    eprintln!("{}", dev_tools::ISSUE_ACHIEVEMENT_USAGE);
                    std::process::exit(1);
                }
            }
        }
        #[cfg(feature = "dev-tools")]
        Some("add-shard-key") => {
            let raw_args: Vec<String> = args.collect();
            match dev_tools::AddShardKeyArgs::parse(&raw_args) {
                Ok(parsed) => dev_tools::add_shard_key(parsed).await,
                Err(message) => {
                    eprintln!("{message}");
                    eprintln!("{}", dev_tools::ADD_SHARD_KEY_USAGE);
                    std::process::exit(1);
                }
            }
        }
        #[cfg(feature = "dev-tools")]
        Some("register-issuer") => {
            let raw_args: Vec<String> = args.collect();
            match dev_tools::RegisterIssuerArgs::parse(&raw_args) {
                Ok(parsed) => dev_tools::register_issuer(parsed).await,
                Err(message) => {
                    eprintln!("{message}");
                    eprintln!("{}", dev_tools::REGISTER_ISSUER_USAGE);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!(
                "usage: avalon <inspect-ledger|inspect-ledger-full|outbox-status|prune-ledger [--dry-run]|rebuild-index|migrate-network --target-database-url <url> --target-network-id <id>|discover-mirror-peers|check-switch-readiness <old-host-url> <new-host-url> [--shard-id <id>] [--verify-key <hex>]|list-equivocations [network_id]|verify-mirror-convergence <network_id> [--shard-id <id>] [--source <url>]|resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--shard-id <id>] [--discard-mirrored]|logs export [<file>] [--file <path>] [--tail <n>] [--since <rfc3339-timestamp>]{}>",
                if cfg!(feature = "dev-tools") {
                    "|create-identity|login <identity_id>|register-integrator|register-game --slug <slug> --name <name> --owner-name <owner> [--capability <cap>]... [--server <url>]|issue-achievement --integrator <slug> --achievement <key> --token <session-token> [--key <path>] [--key-id <uuid>] [--server <url>]|register-issuer --integrator <slug> (--network-id <network_id> | --env <dev|int|mainnet>) [--issuer-ref <ref>] [--key <path>] [--server <url>]|add-shard-key --integrator <slug> [--verify-key <hex>] [--key <path>] [--server <url>]|pair-device"
                } else {
                    ""
                }
            );
            std::process::exit(1);
        }
    }
}

/// `register-integrator` is the primary command name; `register-game`
/// (the original) stays a working deprecated alias so existing scripts
/// keep running. Both route here to the exact same
/// `RegisterIntegratorArgs::parse`/`register_integrator` call — see
/// `docs/projects/backend-server/architecture/issuers.md`.
#[cfg(feature = "dev-tools")]
fn is_register_integrator_command(command: &str) -> bool {
    matches!(command, "register-integrator" | "register-game")
}

async fn outbox_status() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    let status = avalon_server::outbox::status(&pool)
        .await
        .expect("failed to read outbox status");

    match status.oldest_pending {
        None => println!("outbox: empty, nothing pending"),
        Some(oldest) => println!(
            "outbox: {} pending, oldest enqueued at {oldest}",
            status.pending_count
        ),
    }
}

/// `avalon list-equivocations [network_id]` — issue #316, the read side of
/// #300's decided equivocation response procedure. Defaults `network_id` to
/// this node's own genesis network (read the same way `inspect-ledger`
/// does) when not given, since most operators are asking "does *my*
/// network have any open equivocation" rather than watching an arbitrary
/// one. Shows every finding ever recorded, resolved or not — this is the
/// history view; the mirror-watcher's own backfill gate only cares about
/// unresolved ones.
async fn list_equivocations(network_id_arg: Option<String>) {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    let network_id = match network_id_arg {
        Some(id) => id,
        None => PostgresSettlementProvider::read_genesis_network_id(&pool)
            .await
            .expect("failed to read genesis")
            .unwrap_or_else(|| {
                eprintln!(
                    "no genesis set on this node and no network_id given — usage: avalon list-equivocations [network_id]"
                );
                std::process::exit(1);
            }),
    };

    let findings = avalon_chain::mirror::list_equivocations(&pool, &network_id)
        .await
        .expect("failed to read equivocation findings");

    if findings.is_empty() {
        println!("network_id: {network_id} — no equivocation ever recorded");
        return;
    }

    println!("network_id: {network_id} — {} finding(s):", findings.len());
    for f in &findings {
        println!(
            "┌─ shard_id {} tree_size {} ─────────────────────────────────────",
            f.shard_id, f.tree_size
        );
        println!(
            "│ source_a:  {} → root {}",
            f.source_a,
            short_hash(&f.root_hash_a)
        );
        println!(
            "│ source_b:  {} → root {}",
            f.source_b,
            short_hash(&f.root_hash_b)
        );
        match (&f.resolved_at, &f.resolved_root_hash) {
            (Some(at), Some(root)) => println!(
                "│ status:    ✓ resolved at {at} — legitimate root: {}",
                short_hash(root)
            ),
            _ => println!(
                "│ status:    ✗ UNRESOLVED — mirror-watcher refuses to backfill this network \
                 past this point until this is resolved (see docs/projects/backend-server/for-maintainers/equivocation-response.md)"
            ),
        }
        println!("└──────────────────────────────────────────────────────");
    }
}

/// `avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--shard-id <id>] [--discard-mirrored]`
/// — issue #316, the write side of #300's decided equivocation response
/// procedure. Records that an operator has completed the investigation
/// playbook (`docs/projects/backend-server/for-maintainers/equivocation-response.md`) and determined
/// which of the two disagreeing root hashes at `tree_size` was legitimate.
///
/// `--shard-id` (defaults to `"core"`) — every shard under a
/// `network_id` has its own independent `tree_size` numbering, so a
/// finding is only ever identified by `network_id`/`shard_id`/`tree_size`
/// together, not `network_id`/`tree_size` alone (two unrelated shards can
/// and normally will both have an entry at the same `tree_size`).
///
/// `--discard-mirrored` additionally drops any `mirrored_entries` this node
/// already verified at or beyond `tree_size` for this shard
/// (`avalon_chain::mirror::discard_mirrored_entries_from`) — pass it when
/// this node's own mirrored history might include content from the losing
/// branch, so the next mirror-watcher tick re-fetches and re-verifies from
/// a clean point rather than resuming on top of potentially-wrong local
/// state. Safe to omit (and re-run with it later) if unsure; it is not the
/// default because a pure mirror that never actually advanced past
/// `tree_size` has nothing to discard, and unconditionally deleting is
/// needless churn for that (expected to be the more common) case.
async fn resolve_equivocation(raw_args: &[String]) {
    let discard_mirrored = raw_args.iter().any(|a| a == "--discard-mirrored");
    let shard_id = arg_value(raw_args, "--shard-id").unwrap_or_else(|| "core".to_string());
    let shard_id = shard_id.as_str();
    let skip_next = raw_args
        .iter()
        .position(|a| a == "--shard-id")
        .map(|i| i + 1);
    let positional: Vec<&String> = raw_args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && Some(*i) != skip_next)
        .map(|(_, a)| a)
        .collect();
    let [network_id, tree_size_raw, legitimate_root_hash] = positional[..] else {
        eprintln!(
            "usage: avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--shard-id <id>] [--discard-mirrored]"
        );
        std::process::exit(1);
    };
    let Ok(tree_size) = tree_size_raw.parse::<i64>() else {
        eprintln!("tree_size must be an integer, got: {tree_size_raw}");
        std::process::exit(1);
    };

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    let resolved_count = avalon_chain::mirror::resolve_equivocation(
        &pool,
        network_id,
        shard_id,
        tree_size,
        legitimate_root_hash,
    )
    .await
    .expect("failed to resolve equivocation finding(s)");
    if resolved_count == 0 {
        println!(
            "no unresolved finding at network_id={network_id} shard_id={shard_id} tree_size={tree_size} — nothing to do \
             (already resolved, or never existed)"
        );
        return;
    }
    println!(
        "resolved {resolved_count} finding(s) at network_id={network_id} shard_id={shard_id} tree_size={tree_size}: \
         legitimate root is {}",
        short_hash(legitimate_root_hash)
    );

    if discard_mirrored {
        let discarded = avalon_chain::mirror::discard_mirrored_entries_from(
            &pool, network_id, shard_id, tree_size,
        )
        .await
        .expect("failed to discard mirrored entries");
        println!(
            "discarded {discarded} locally-mirrored entr(ies) verified at or beyond tree_size={tree_size} \
             — the mirror-watcher will re-fetch and re-verify them from the now-resolved branch on its next tick"
        );
    } else {
        println!(
            "--discard-mirrored not passed — this node's own mirrored_entries beyond tree_size={tree_size} \
             were left untouched; re-run with --discard-mirrored if this node's local history might include \
             content from the losing branch"
        );
    }
}

/// `avalon prune-ledger [--dry-run]` — issue #208's operator-facing entry
/// point for node-tiered retention pruning. Reads `AVALON_RETENTION_*` from
/// the environment (`avalon_chain::retention::RetentionConfig::from_env`)
/// and does exactly, and only, what that config says:
///
/// - `full` tier, or `hot` tier with pruning left disabled: reports the
///   config and exits without touching anything — running this command is
///   always safe regardless of configuration.
/// - `hot` tier with `AVALON_RETENTION_PRUNING_ENABLED=true`: reports how
///   many entries are prunable right now, then actually prunes them,
///   unless `--dry-run` was passed, in which case it stops after
///   reporting the count.
///
/// This is deliberately a manual/cron-invoked command rather than only a
/// background loop — see `crates/server/src/retention.rs::run_worker` for
/// the periodic in-process version `avalon-server` runs when its own
/// config enables pruning; both call the same
/// `avalon_chain::PostgresSettlementProvider::prune_payloads_older_than`.
///
/// **Issue #569: does not perform archive-confirmation gating itself.**
/// `avalon-cli`'s `--no-default-features` build has no `reqwest`
/// dependency at all (see this crate's own feature-split doc comment) —
/// this command can't make the HTTP calls that check would need. A real
/// (non-`--dry-run`) prune refuses outright when the loaded config
/// requires archive confirmation (`AVALON_RETENTION_ARCHIVE_PEERS` set),
/// pointing the operator at `avalon-server`'s background worker instead —
/// the one place that check actually runs — rather than silently skipping
/// a safety gate the config asked for.
async fn prune_ledger(dry_run: bool) {
    let config = avalon_chain::retention::RetentionConfig::from_env().unwrap_or_else(|e| {
        eprintln!("invalid retention configuration: {e}");
        std::process::exit(1);
    });
    println!("retention tier: {}", config.describe());

    let Some(cutoff) = config.prune_cutoff(time::OffsetDateTime::now_utc()) else {
        println!("nothing to do (either full tier, or hot tier with pruning disabled)");
        return;
    };

    if !dry_run && config.requires_archive_confirmation() {
        eprintln!(
            "refusing to prune: this configuration requires archive confirmation \
             (AVALON_RETENTION_ARCHIVE_PEERS is set), which this manual command cannot check \
             (no reqwest dependency in a --no-default-features build). Use avalon-server's \
             background retention worker instead (AVALON_RETENTION_PRUNING_ENABLED=true on a \
             running avalon-server), or run with --dry-run to see counts without pruning."
        );
        std::process::exit(1);
    }

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");
    let network_id = PostgresSettlementProvider::read_genesis_network_id(&pool)
        .await
        .expect("failed to read genesis")
        .unwrap_or_else(|| "(no genesis set)".to_string());
    let chain = PostgresSettlementProvider::new(pool, network_id);

    let prunable = chain
        .prunable_entry_count(cutoff)
        .await
        .expect("failed to count prunable entries");
    println!("cutoff: {cutoff} — {prunable} entries currently prunable");

    if dry_run {
        println!("--dry-run passed: not pruning anything");
        return;
    }
    if prunable == 0 {
        println!("nothing to prune");
        return;
    }

    let report = chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("failed to prune ledger payloads");
    println!(
        "pruned {} entries' payloads (kept entry_hash/prev_hash/seq/batch_id intact)",
        report.pruned_count
    );
}

/// `avalon rebuild-index` — issue #43's operator-facing entry point for the
/// disaster-recovery rebuild: truncates every projection table and
/// replays the entire `ledger_entries` history back through the indexer.
/// Safe to run against a live database (the ordinary write path is
/// unaffected — the indexer's own dedup table is truncated too, so
/// replayed events aren't silently skipped as "already applied"), but a
/// concurrent read against a partially-rebuilt projection will see
/// transiently empty/incomplete state; run during a maintenance window on
/// a real deployment, same caveat `avalon prune-ledger` already carries
/// for a different reason.
async fn rebuild_index() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");
    let network_id = PostgresSettlementProvider::read_genesis_network_id(&pool)
        .await
        .expect("failed to read genesis")
        .unwrap_or_else(|| "(no genesis set)".to_string());
    let chain = PostgresSettlementProvider::new(pool.clone(), network_id);

    let started = std::time::Instant::now();
    let report = avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool)
        .await
        .expect("failed to rebuild index from ledger");
    let elapsed = started.elapsed();

    println!(
        "rebuilt index from {} ledger entries ({} events applied, {} skipped as undecodable) in {:.2?}",
        report.entries_read, report.events_applied, report.entries_skipped_undecodable, elapsed
    );
}

/// `avalon migrate-network --target-database-url <url> --target-network-id <id>`
/// — issue #484's operator entry point for a deliberate
/// `avalon-mainnet-N` -> `avalon-mainnet-(N+1)` genesis reset (per #476/
/// #479's decision). Runs against this process's own `DATABASE_URL` as the
/// *source* network and cuts over onto `--target-database-url`, which must
/// already have migrations applied (`make migrate` against it) but no
/// genesis of its own yet — `avalon_chain::migration::migrate_network`
/// creates that genesis as part of the cutover. Safe to re-run after a
/// partial failure; see that function's own doc comment for why.
async fn migrate_network(raw_args: &[String]) {
    let target_database_url = arg_value(raw_args, "--target-database-url").unwrap_or_else(|| {
        eprintln!(
            "usage: avalon migrate-network --target-database-url <url> --target-network-id <id>"
        );
        std::process::exit(1);
    });
    let target_network_id = arg_value(raw_args, "--target-network-id").unwrap_or_else(|| {
        eprintln!(
            "usage: avalon migrate-network --target-database-url <url> --target-network-id <id>"
        );
        std::process::exit(1);
    });

    let source_database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let source_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&source_database_url)
        .await
        .expect("failed to connect to source Postgres (DATABASE_URL)");
    let target_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&target_database_url)
        .await
        .expect("failed to connect to target Postgres (--target-database-url)");

    let report =
        avalon_chain::migration::migrate_network(&source_pool, &target_pool, &target_network_id)
            .await
            .unwrap_or_else(|e| {
                eprintln!("migration failed: {e}");
                std::process::exit(1);
            });

    println!(
        "migrated {} (tree_size {}) -> {}{}",
        report.source_network_id,
        report.source_tree_size,
        report.target_network_id,
        if report.checkpoint_already_recorded {
            " (checkpoint already recorded — this network was already migrated from this source; re-run is idempotent)"
        } else {
            ""
        }
    );
    println!(
        "carried over {} issuer registration(s) — no re-registration or re-signing required",
        report.issuers_carried_over
    );
}

/// Looks up `--flag <value>` in a raw CLI arg slice — shared by any command
/// here that takes named flags instead of positional args.
fn arg_value(raw_args: &[String], flag: &str) -> Option<String> {
    raw_args
        .iter()
        .position(|a| a == flag)
        .and_then(|i| raw_args.get(i + 1))
        .cloned()
}

/// `avalon discover-mirror-peers` — issue #511. Closes the gap between
/// #362's node-to-node announce/bootstrap discovery and `AVALON_MIRROR_PEERS`
/// (a fully separate, manually-set env var with no fallback of its own): a
/// hoster who successfully discovers peers previously still had to
/// hand-copy URLs themselves, with no guidance on which ones exist to
/// choose from. `make stack-up`'s first-run `.env` generation invokes this
/// (via the `discover-mirror-peers` Compose service, so no host Rust
/// toolchain is needed — see `docker-compose.yml`) and appends whatever
/// this prints on stdout to the generated `.env`'s `AVALON_MIRROR_PEERS`.
///
/// **Every path below that has nothing useful to offer prints nothing and
/// exits 0** — this must never block a non-interactive bring-up:
/// `AVALON_MIRROR_PEERS` already set, no bootstrap peer configured/resolved,
/// the bootstrap peer unreachable, its address book empty, or stdin isn't a
/// real terminal (a non-interactive `docker compose run` — e.g. CI, or a
/// scripted bring-up — passes `-T` and gets no TTY, so this skips the
/// prompt exactly like every other unattended case here). All prompts and
/// status text go to stderr, so stdout only ever carries the final
/// comma-separated peer list (or nothing) — safe for the Makefile to
/// capture directly.
///
/// **"ALL" is never a silent default** — mirroring another node's data is a
/// deliberate trust decision (`docs/projects/backend-server/architecture/nodes.md`'s node-authority
/// model), not something that should happen just because a peer showed up
/// in discovery. An empty selection (just pressing enter) mirrors nothing,
/// same as skipping the prompt entirely.
async fn discover_mirror_peers() {
    if std::env::var("AVALON_MIRROR_PEERS")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return;
    }

    let network_id = std::env::var("AVALON_NETWORK_ID").unwrap_or_default();
    let bootstrap_peers = avalon_server::nodes::resolve_bootstrap_peers(
        std::env::var("AVALON_BOOTSTRAP_PEERS").ok().as_deref(),
        &network_id,
        avalon_protocol::network_trust::bundled_trust_anchors(),
    );
    let Some(bootstrap_peer) = bootstrap_peers.into_iter().next() else {
        return;
    };

    let own_base_url = std::env::var("AVALON_NODE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());

    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(_) => return,
    };

    let Ok(response) = http
        .get(format!("{bootstrap_peer}/nodes/peers"))
        .send()
        .await
    else {
        eprintln!(
            "discover-mirror-peers: bootstrap peer {bootstrap_peer} unreachable — skipping mirror-peer discovery"
        );
        return;
    };
    let Ok(peers) = response.json::<Vec<avalon_server::nodes::PeerInfo>>().await else {
        return;
    };

    // The bootstrap peer itself is a real, reachable candidate too — not
    // just whatever it happens to already know about — so it's included
    // alongside its address book, deduplicated, and never offered as a
    // candidate to mirror itself.
    let mut candidates: Vec<String> = std::iter::once(bootstrap_peer)
        .chain(peers.into_iter().map(|p| p.base_url))
        .filter(|url| Some(url) != own_base_url.as_ref())
        .collect();
    candidates.sort();
    candidates.dedup();

    if candidates.is_empty() {
        return;
    }

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return;
    }

    eprintln!("discover-mirror-peers: discovered peer(s) available to mirror:");
    for (i, url) in candidates.iter().enumerate() {
        eprintln!("  {}) {url}", i + 1);
    }
    eprintln!(
        "Select peer(s) to mirror (comma-separated numbers, ALL for every peer above, or blank for none):"
    );

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return;
    }

    let selected = parse_peer_selection(&input, &candidates);
    if !selected.is_empty() {
        println!("{}", selected.join(","));
    }
}

/// `discover_mirror_peers`'s selection parsing, split out for direct unit
/// testing — `"ALL"`/`"all"` selects every candidate, comma-separated
/// 1-indexed numbers select specific ones (any out-of-range or unparseable
/// entry is silently dropped rather than erroring, since a hoster mistyping
/// one number in a list shouldn't lose the rest of their selection), and
/// anything else (in particular a blank line) selects nothing — "ALL" is
/// never applied by default, only on this exact explicit input.
fn parse_peer_selection(input: &str, candidates: &[String]) -> Vec<String> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return candidates.to_vec();
    }
    input
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1 && *n <= candidates.len())
        .map(|n| candidates[n - 1].clone())
        .collect()
}

/// A remote `GET /ledger/sth/*` response, trimmed to what
/// `check_switch_readiness` needs — deliberately not
/// `avalon_protocol::sth::SignedTreeHead` itself (that type has no
/// `Deserialize`; it's only ever built in-process from a real signature,
/// never trusted in from the wire, everywhere else this crate uses it).
/// This command is the one place a `SignedTreeHead`-shaped value
/// legitimately arrives over HTTP from a party this process doesn't
/// control — converted immediately into the real type below so
/// `avalon_protocol::sth::verify_tree_head` (the same signature check every
/// other verifier in this codebase uses) can check it like any other.
#[derive(Debug, Clone, serde::Deserialize)]
struct RemoteSth {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
}

impl From<RemoteSth> for avalon_protocol::sth::SignedTreeHead {
    fn from(r: RemoteSth) -> Self {
        avalon_protocol::sth::SignedTreeHead {
            tree_size: r.tree_size,
            root_hash: r.root_hash,
            network_id: r.network_id,
            signing_key_id: r.signing_key_id,
            signature: r.signature,
            created_at: r.created_at,
        }
    }
}

/// `avalon check-switch-readiness <old-host-url> <new-host-url> [--shard-id
/// <id>] [--verify-key <hex>]` — issue #544's switch-host tooling: the
/// design decided a managed-hosting integrator is never cryptographically
/// locked to one host (the shard's identity is the integrator's own key,
/// #543, never the host's) and can always switch to a different host or
/// to self-hosting by having the new one sync the shard's existing history
/// from any mirror — but nothing concrete existed to answer the actual
/// operational question an integrator faces mid-switch: **has the new host
/// actually caught up, and does it agree with the old one, before I cut
/// traffic over?** This answers exactly that, read-only, against real
/// running nodes:
///
/// 1. Fetches `old-host`'s latest STH for `--shard-id` (default `core`).
///    If `old-host` is unreachable — plausible, it may be the very host
///    that's stalling — falls back to reporting `new-host`'s own latest
///    STH alone, with an explicit `UNKNOWN` verdict rather than a false
///    `READY`.
/// 2. Fetches `new-host`'s STH at *exactly* `old-host`'s `tree_size` for
///    the same shard. A 404 there means `new-host` hasn't backfilled that
///    far yet — `NOT_READY`. A different `root_hash` at the same
///    `tree_size` is `MISMATCH` — a serious finding (the two hosts
///    disagree about the shard's actual history at a point both claim to
///    have), never silently treated as "close enough."
/// 3. Equal root hashes at that `tree_size` is `READY` — the same
///    self-verifying comparison every mirror in this codebase already
///    does, just run manually against two specific hosts on demand.
/// 4. `--verify-key` (the shard's registered verify key, hex-encoded) is
///    optional but recommended — when given, both STHs' signatures are
///    also checked with `avalon_protocol::sth::verify_tree_head`, the same
///    check `inspect-ledger` runs locally. Without it, a `READY` verdict
///    only means "these two hosts' claims agree with each other," not
///    "both are honest" — the whole reason this crate's convention is
///    "never trust, verify" rather than assuming agreement implies
///    correctness.
///
/// Never mutates anything — this is purely a pre-cutover diagnostic. What
/// to actually *do* with a `READY` verdict (updating
/// `AVALON_SETTLEMENT_REMOTE_URLS`/self-hosting config to point at the new
/// host) stays a manual, deliberate operator action, same as every other
/// deployment-config change in this repo.
async fn check_switch_readiness(raw_args: &[String]) {
    const USAGE: &str = "usage: avalon check-switch-readiness <old-host-url> <new-host-url> [--shard-id <id>] [--verify-key <hex>]";

    let mut shard_id = "core".to_string();
    let mut verify_key_hex: Option<String> = None;
    let mut positional = Vec::new();
    let mut iter = raw_args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--shard-id" => match iter.next() {
                Some(v) => shard_id = v.clone(),
                None => {
                    eprintln!("--shard-id requires a value\n{USAGE}");
                    std::process::exit(1);
                }
            },
            "--verify-key" => match iter.next() {
                Some(v) => verify_key_hex = Some(v.clone()),
                None => {
                    eprintln!("--verify-key requires a value\n{USAGE}");
                    std::process::exit(1);
                }
            },
            other => positional.push(other.to_string()),
        }
    }
    let [old_host, new_host] = positional.as_slice() else {
        eprintln!("{USAGE}");
        std::process::exit(1);
    };
    let old_host = old_host.trim_end_matches('/');
    let new_host = new_host.trim_end_matches('/');

    let verify_key = verify_key_hex.map(|hex_value| {
        let bytes = hex::decode(&hex_value).unwrap_or_else(|e| {
            eprintln!("--verify-key is not valid hex: {e}");
            std::process::exit(1);
        });
        let bytes: [u8; 32] = bytes.try_into().unwrap_or_else(|v: Vec<u8>| {
            eprintln!(
                "--verify-key must decode to exactly 32 bytes, got {}",
                v.len()
            );
            std::process::exit(1);
        });
        ed25519_dalek::VerifyingKey::from_bytes(&bytes).unwrap_or_else(|e| {
            eprintln!("--verify-key is not a valid Ed25519 public key: {e}");
            std::process::exit(1);
        })
    });

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    let report_sth = |label: &str, sth: &avalon_protocol::sth::SignedTreeHead| {
        println!(
            "{label}: tree_size={} root_hash={}",
            sth.tree_size, sth.root_hash
        );
        if let Some(key) = &verify_key {
            let ok = avalon_protocol::sth::verify_tree_head(key, sth);
            println!(
                "{label}: signature {}",
                if ok {
                    "VERIFIES against --verify-key"
                } else {
                    "DOES NOT VERIFY against --verify-key"
                }
            );
            if !ok {
                println!(
                    "{label}: WARNING — do not trust this host's claim until this is resolved"
                );
            }
        }
    };

    let old_sth: Option<avalon_protocol::sth::SignedTreeHead> = match http
        .get(format!("{old_host}/ledger/sth/latest?shard_id={shard_id}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<RemoteSth>().await {
            Ok(sth) => Some(sth.into()),
            Err(e) => {
                println!("old-host ({old_host}): returned an unparseable response — {e}");
                None
            }
        },
        Ok(resp) => {
            println!(
                "old-host ({old_host}): {} for shard '{shard_id}' — treating as unreachable/no data",
                resp.status()
            );
            None
        }
        Err(e) => {
            println!("old-host ({old_host}): unreachable — {e}");
            None
        }
    };

    let Some(old_sth) = old_sth else {
        // old-host may genuinely be the stalled party this whole check
        // exists for — fall back to reporting new-host's own state alone
        // rather than refusing to answer anything.
        match http
            .get(format!("{new_host}/ledger/sth/latest?shard_id={shard_id}"))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => match resp.json::<RemoteSth>().await {
                Ok(sth) => report_sth("new-host", &sth.into()),
                Err(e) => println!("new-host ({new_host}): returned an unparseable response — {e}"),
            },
            Ok(resp) => println!(
                "new-host ({new_host}): {} for shard '{shard_id}'",
                resp.status()
            ),
            Err(e) => println!("new-host ({new_host}): unreachable — {e}"),
        }
        println!(
            "VERDICT: UNKNOWN — could not reach old-host to confirm new-host has actually caught up"
        );
        return;
    };
    report_sth("old-host", &old_sth);

    let new_sth: Option<avalon_protocol::sth::SignedTreeHead> = match http
        .get(format!(
            "{new_host}/ledger/sth/{}?shard_id={shard_id}",
            old_sth.tree_size
        ))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<RemoteSth>().await {
            Ok(sth) => Some(sth.into()),
            Err(e) => {
                println!("new-host ({new_host}): returned an unparseable response — {e}");
                None
            }
        },
        Ok(resp) => {
            println!(
                "new-host ({new_host}): {} at tree_size {} for shard '{shard_id}' — not caught up yet",
                resp.status(),
                old_sth.tree_size
            );
            None
        }
        Err(e) => {
            println!("new-host ({new_host}): unreachable — {e}");
            None
        }
    };

    let Some(new_sth) = new_sth else {
        println!(
            "{}",
            switch_verdict(&old_sth.root_hash, None).describe(old_sth.tree_size)
        );
        return;
    };
    report_sth("new-host", &new_sth);

    println!(
        "{}",
        switch_verdict(&old_sth.root_hash, Some(&new_sth.root_hash)).describe(old_sth.tree_size)
    );
}

/// `avalon verify-mirror-convergence <network_id> [--shard-id <id>] [--source <url>]`
/// — an offline check that this node's mirrored copy of a shard is the
/// authority's complete, unbroken history, using only what this node
/// already stored: recomputes the Merkle root over every mirrored entry
/// and requires it to equal an observed, signature-verified STH at the
/// furthest `tree_size` any peer attested to, with an intact hash chain and
/// no open equivocation. Needs no connection to the authority, so it works
/// while the authority is down. Exits 0 only when converged.
async fn verify_mirror_convergence(raw_args: &[String]) {
    const USAGE: &str =
        "usage: avalon verify-mirror-convergence <network_id> [--shard-id <id>] [--source <url>]";
    let mut network_id: Option<String> = None;
    let mut shard_id = avalon_chain::mirror::CORE_SHARD_ID.to_string();
    let mut source: Option<String> = None;
    let mut iter = raw_args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--shard-id" => shard_id = iter.next().cloned().unwrap_or_else(|| usage_exit(USAGE)),
            "--source" => source = Some(iter.next().cloned().unwrap_or_else(|| usage_exit(USAGE))),
            other if !other.starts_with("--") && network_id.is_none() => {
                network_id = Some(other.to_string())
            }
            _ => usage_exit(USAGE),
        }
    }
    let network_id = network_id.unwrap_or_else(|| usage_exit(USAGE));

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");
    let report =
        avalon_chain::mirror::check_convergence(&pool, &network_id, &shard_id, source.as_deref())
            .await
            .expect("failed to check mirror convergence");

    println!("network_id: {network_id}  shard_id: {shard_id}");
    println!("mirrored entries:     {}", report.mirrored_count);
    match &report.recomputed_root {
        Some(root) => println!("recomputed root:      {}", short_hash(root)),
        None => println!("recomputed root:      (nothing mirrored)"),
    }
    match &report.latest_observed {
        Some(sth) => println!(
            "furthest observed STH: tree_size {} root {} (source {}, key {})",
            sth.tree_size,
            short_hash(&sth.root_hash),
            sth.source_url,
            sth.signing_key_id
        ),
        None => println!("furthest observed STH: (none)"),
    }
    println!(
        "hash chain:           {}",
        match report.chain_breaks.first() {
            None => "unbroken".to_string(),
            Some(first) => format!(
                "{} broken link(s), first at seq {first}",
                report.chain_breaks.len()
            ),
        }
    );
    println!("open equivocations:   {}", report.unresolved_equivocations);

    let verdict = report.verdict;
    println!("{}", describe_convergence(&verdict));
    if !matches!(
        verdict,
        avalon_chain::mirror::ConvergenceVerdict::Converged { .. }
    ) {
        std::process::exit(2);
    }
}

fn usage_exit(usage: &str) -> ! {
    eprintln!("{usage}");
    std::process::exit(1);
}

fn describe_convergence(verdict: &avalon_chain::mirror::ConvergenceVerdict) -> String {
    use avalon_chain::mirror::ConvergenceVerdict as V;
    match verdict {
        V::Converged { tree_size } => format!(
            "VERDICT: CONVERGED — the mirrored history matches an observed STH at tree_size {tree_size}"
        ),
        V::NothingMirrored => "VERDICT: NOTHING_MIRRORED — this node holds no mirrored entries for this shard".to_string(),
        V::Behind { mirrored, observed } => format!(
            "VERDICT: BEHIND — {mirrored} entries mirrored but an STH attests to tree_size {observed}; the last {} entries are missing from this node",
            observed - mirrored
        ),
        V::NoObservedSth => "VERDICT: NO_OBSERVED_STH — no signed tree head was ever observed for this shard, so there is nothing to verify the mirrored data against".to_string(),
        V::RootMismatch { tree_size } => format!(
            "VERDICT: ROOT_MISMATCH — no observed STH at tree_size {tree_size} has the root recomputed from the mirrored entries; do not promote (see docs/projects/backend-server/for-maintainers/equivocation-response.md)"
        ),
        V::ChainBroken { breaks } => format!(
            "VERDICT: CHAIN_BROKEN — {breaks} mirrored entr(ies) do not link to their predecessor; do not promote"
        ),
        V::BlockedByEquivocation { findings } => format!(
            "VERDICT: BLOCKED — {findings} unresolved equivocation finding(s); resolve them first (see docs/projects/backend-server/for-maintainers/equivocation-response.md)"
        ),
    }
}

/// The three possible outcomes of comparing `new-host`'s root hash at
/// `old-host`'s `tree_size` against `old-host`'s own root hash — pure
/// decision logic, split out from `check_switch_readiness`'s I/O for
/// direct unit testing (same "pure function behind the env/IO wrapper"
/// pattern this repo already uses elsewhere, e.g. `nodes::resolve_bootstrap_peers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchVerdict {
    /// `new-host` hasn't backfilled up to `old-host`'s `tree_size` yet.
    NotReady,
    /// Both hosts agree on the root hash at that exact `tree_size`.
    Ready,
    /// Both hosts claim a value at the exact same `tree_size` but
    /// disagree on the root hash — never "close enough," always a
    /// serious finding worth investigating before switching.
    Mismatch,
}

impl SwitchVerdict {
    fn describe(self, old_tree_size: i64) -> String {
        match self {
            SwitchVerdict::NotReady => format!(
                "VERDICT: NOT_READY — new-host has not synced up to old-host's tree_size {old_tree_size} yet"
            ),
            SwitchVerdict::Ready => format!(
                "VERDICT: READY — old-host and new-host agree on the shard's history up to tree_size {old_tree_size}"
            ),
            SwitchVerdict::Mismatch => format!(
                "VERDICT: MISMATCH — old-host and new-host report DIFFERENT root hashes at the SAME tree_size {old_tree_size} — do not switch, investigate before proceeding (see docs/projects/backend-server/for-maintainers/equivocation-response.md)"
            ),
        }
    }
}

fn switch_verdict(
    old_root_hash: &str,
    new_root_hash_at_old_tree_size: Option<&str>,
) -> SwitchVerdict {
    match new_root_hash_at_old_tree_size {
        None => SwitchVerdict::NotReady,
        Some(new_root) if new_root == old_root_hash => SwitchVerdict::Ready,
        Some(_) => SwitchVerdict::Mismatch,
    }
}

/// `full: false` is `avalon inspect-ledger` — the concise chain-integrity
/// view. `full: true` is `avalon inspect-ledger-full` — the same view plus
/// each entry's actual payload (pretty-printed JSON) and version, for
/// answering "what's actually inside this block" rather than just "is the
/// chain intact." Same query either way (`list_entries` always fetches the
/// payload, since it needs it to re-verify each entry's hash) — this only
/// changes what gets printed.
///
/// Prints a batch boundary header whenever the entry stream
/// crosses into a new `batch_id`, showing that batch's seq range and root —
/// entries within a batch stay hash-chained exactly as before, this only
/// adds where the batch lines are drawn.
async fn inspect_ledger(full: bool) {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    // Read-only: reports whatever genesis is already there
    // without creating or asserting one — an operator pointing this at an
    // unfamiliar database should see which network it belongs to before
    // anything else.
    let network_id = PostgresSettlementProvider::read_genesis_network_id(&pool)
        .await
        .expect("failed to read genesis")
        .unwrap_or_else(|| "(no genesis set)".to_string());
    println!("network_id: {network_id}");

    let chain = PostgresSettlementProvider::new(pool, network_id);
    let entries = chain.list_entries().await.expect("failed to read ledger");
    let batches = chain.list_batches().await.expect("failed to read batches");

    if entries.is_empty() {
        println!("(ledger is empty)");
        return;
    }

    let batch_by_id: std::collections::HashMap<Uuid, &avalon_chain::LedgerBatchView> =
        batches.iter().map(|b| (b.batch_id, b)).collect();

    let mut current_batch: Option<Uuid> = None;
    for entry in &entries {
        if current_batch != Some(entry.batch_id) {
            current_batch = Some(entry.batch_id);
            match batch_by_id.get(&entry.batch_id) {
                Some(batch) => println!(
                    "═══ Batch {} — seq {}-{} — root: {} ═══",
                    batch.batch_id,
                    batch.first_seq,
                    batch.last_seq,
                    short_hash(&batch.batch_root)
                ),
                None => println!("═══ Batch {} (no ledger_batches row) ═══", entry.batch_id),
            }
        }

        let verified = if !entry.chain_intact {
            "✗ BROKEN CHAIN"
        } else if entry.payload_pruned {
            // Issue #208: a pruned entry's link is still intact — its
            // content just isn't independently re-checkable from this
            // node any more. This is never reported as broken; it's a
            // distinct, honest third state.
            "✓ (payload pruned — content not locally re-verifiable)"
        } else {
            "✓"
        };
        println!(
            "┌─ Block #{} ─────────────────────────────────────────",
            entry.seq
        );
        println!("│ kind:      {}", entry.kind);
        println!("│ issuer:    {}", entry.issuer);
        println!("│ subject:   {}", entry.subject);
        println!("│ timestamp: {}", entry.event_timestamp);
        if full {
            println!("│ version:   {}", entry.version);
            match &entry.payload {
                Some(payload) => {
                    let pretty = serde_json::to_string_pretty(payload)
                        .unwrap_or_else(|_| payload.to_string());
                    println!("│ payload:");
                    for line in pretty.lines() {
                        println!("│   {line}");
                    }
                }
                None => println!("│ payload:   [pruned — see AVALON_RETENTION_* in .env.example]"),
            }
        }
        println!("│ hash:      {}", short_hash(&entry.entry_hash));
        println!("│ prev:      {}", short_hash(&entry.prev_hash));
        println!("│ verified:  {verified}");
        println!("└──────────────────────────────────────────────────────");
    }

    let broken = entries.iter().filter(|e| !e.chain_intact).count();
    let pruned = entries.iter().filter(|e| e.payload_pruned).count();
    println!();
    println!(
        "{} entries across {} batch(es), {}",
        entries.len(),
        batches.len(),
        if broken == 0 {
            "chain intact ✓".to_string()
        } else {
            format!("{broken} broken link(s) ✗")
        }
    );
    // Issue #208: tell an operator whether they're looking at a full node
    // or a (partially) pruned hot-tier node, in-band with everything else
    // this command already reports — derived from the data itself
    // (payload_pruned_at), not from this process's own env config, so it's
    // accurate no matter which node's database this is pointed at.
    if pruned == 0 {
        println!("retention: full — every entry's payload is present");
    } else {
        println!(
            "retention: hot-tier / pruned — {pruned} of {} entries have had their payload pruned locally (still fully present in every other node/mirror the network guarantees, or, at milestone-1 scale with one settlement database, permanently gone — see docs/projects/backend-server/architecture/nodes.md)",
            entries.len()
        );
    }

    // Signed Tree Head verification — a second, independent
    // tamper-evidence layer on top of the hash-chain check above: the
    // latest Merkle root is recomputed fresh from every stored entry_hash
    // and checked against what was actually signed, then that signature is
    // checked against the configured operator public key. Either failure is
    // reported with the same severity as a broken hash-chain link — this is
    // exactly the kind of thing that must never be silently accepted.
    let signed_tree_heads = chain
        .list_signed_tree_heads()
        .await
        .expect("failed to read signed tree heads");
    println!();
    match signed_tree_heads.last() {
        None => println!("signed tree head: (none yet)"),
        Some(sth) => {
            let root_matches = merkle_root_matches(&entries, sth);
            println!(
                "signed tree head @ size {}: root {}",
                sth.tree_size,
                short_hash(&sth.root_hash)
            );
            println!(
                "  merkle recompute: {}",
                if root_matches {
                    "✓".to_string()
                } else {
                    "✗ TAMPER EVIDENCE — recomputed root does not match the signed root".to_string()
                }
            );
            match avalon_protocol::sth::load_verify_key_from_env() {
                Ok(verify_key) => {
                    let signature_valid = avalon_protocol::sth::verify_tree_head(&verify_key, sth);
                    println!(
                        "  signature (key {}): {}",
                        sth.signing_key_id,
                        if signature_valid {
                            "✓".to_string()
                        } else {
                            "✗ TAMPER EVIDENCE — STH signature invalid for the configured verify key".to_string()
                        }
                    );
                }
                Err(e) => println!("  signature: unable to verify — {e}"),
            }
        }
    }
}

/// Recomputes the RFC 6962 Merkle Tree Hash of every `entries` entry up to
/// `sth.tree_size` and checks it against `sth.root_hash` — the structural
/// half of STH verification, independent of the signature check
/// (`avalon_protocol::sth::verify_tree_head`). Pure and directly unit-testable
/// without Postgres; `inspect_ledger` is its only real caller.
/// `entries` must already be ordered by `seq` ascending (as
/// `PostgresSettlementProvider::list_entries` returns them). Takes the
/// first `sth.tree_size` entries by *position*, never by filtering on
/// `e.seq <= sth.tree_size` — `seq` is `GENERATED ALWAYS AS IDENTITY`, and
/// Postgres identity/sequence advancement is not transactional, so a batch
/// commit that inserts rows and then rolls back (a retried outbox drain, a
/// transient failure) permanently burns whatever `seq` values it already
/// allocated. Once a gap like that exists, `tree_size` (a real leaf count —
/// see `crates/chain/src/postgres.rs`'s module doc comment) is no longer
/// equal to "the highest surviving `seq` value that's `<= tree_size`", so a
/// seq-value filter silently drops the newest legitimately-committed
/// entries and recomputes the wrong root — a false "tamper evidence" alarm
/// against a perfectly healthy ledger, the exact failure mode `avalon
/// inspect-ledger` exists to never produce.
fn merkle_root_matches(
    entries: &[avalon_chain::LedgerEntryView],
    sth: &avalon_protocol::sth::SignedTreeHead,
) -> bool {
    let tree_size = sth.tree_size.max(0) as usize;
    let hashes: Vec<String> = entries
        .iter()
        .take(tree_size)
        .map(|e| e.entry_hash.clone())
        .collect();
    avalon_chain::merkle::mth_of_hex_hashes(&hashes)
        .map(hex::encode)
        .map(|root| root == sth.root_hash)
        .unwrap_or(false)
}

fn short_hash(hash: &str) -> String {
    if hash.len() <= 16 {
        hash.to_string()
    } else {
        format!("{}...{}", &hash[..8], &hash[hash.len() - 8..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_chain::LedgerEntryView;
    use avalon_protocol::sth::sign_tree_head;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    #[cfg(feature = "dev-tools")]
    #[test]
    fn register_game_stays_a_working_alias_for_register_integrator() {
        assert!(is_register_integrator_command("register-integrator"));
        assert!(is_register_integrator_command("register-game"));
        assert!(!is_register_integrator_command("register-app"));
    }

    fn sample_candidates() -> Vec<String> {
        vec![
            "http://peer-a:8080".to_string(),
            "http://peer-b:8080".to_string(),
        ]
    }

    /// Issue #511: a blank line (just pressing enter) selects nothing — the
    /// prompt-skip/no-op default, never "ALL" applied silently.
    #[test]
    fn blank_selection_mirrors_nothing() {
        assert!(parse_peer_selection("", &sample_candidates()).is_empty());
        assert!(parse_peer_selection("\n", &sample_candidates()).is_empty());
    }

    #[test]
    fn numeric_selection_picks_the_named_candidates() {
        assert_eq!(
            parse_peer_selection("2", &sample_candidates()),
            vec!["http://peer-b:8080".to_string()]
        );
        assert_eq!(
            parse_peer_selection("1,2", &sample_candidates()),
            sample_candidates()
        );
    }

    /// Out-of-range/unparseable entries are dropped, not fatal — one typo
    /// shouldn't lose the rest of an otherwise-valid selection.
    #[test]
    fn invalid_entries_are_silently_dropped_not_fatal() {
        assert_eq!(
            parse_peer_selection("1,99,nonsense", &sample_candidates()),
            vec!["http://peer-a:8080".to_string()]
        );
    }

    /// "ALL" (any case) is the one explicit way to select every candidate —
    /// never the default for an empty/invalid input.
    #[test]
    fn all_selects_every_candidate_explicitly() {
        assert_eq!(
            parse_peer_selection("ALL", &sample_candidates()),
            sample_candidates()
        );
        assert_eq!(
            parse_peer_selection("all", &sample_candidates()),
            sample_candidates()
        );
    }

    /// Issue #544: new-host hasn't backfilled to old-host's tree_size yet.
    #[test]
    fn switch_verdict_not_ready_when_new_host_has_no_data_at_that_tree_size() {
        assert_eq!(switch_verdict("abc123", None), SwitchVerdict::NotReady);
    }

    /// Both hosts agree — safe to switch.
    #[test]
    fn switch_verdict_ready_when_root_hashes_match() {
        assert_eq!(
            switch_verdict("abc123", Some("abc123")),
            SwitchVerdict::Ready
        );
    }

    /// Same tree_size, different root hash — never "close enough," always
    /// a serious finding.
    #[test]
    fn switch_verdict_mismatch_when_root_hashes_differ_at_the_same_tree_size() {
        assert_eq!(
            switch_verdict("abc123", Some("def456")),
            SwitchVerdict::Mismatch
        );
    }

    fn sample_entries(hashes: &[&str]) -> Vec<LedgerEntryView> {
        hashes
            .iter()
            .enumerate()
            .map(|(i, hash)| LedgerEntryView {
                seq: (i + 1) as i64,
                event_id: Uuid::new_v4(),
                kind: "test.event".to_string(),
                issuer: "identity:x:self:test_event".to_string(),
                subject: "identity:y:self:test_event".to_string(),
                payload: Some(json!({})),
                payload_pruned: false,
                version: 1,
                event_timestamp: time::OffsetDateTime::UNIX_EPOCH,
                prev_hash: "0".repeat(64),
                entry_hash: hash.to_string(),
                batch_id: Uuid::new_v4(),
                chain_intact: true,
            })
            .collect()
    }

    /// Issue #210's `inspect_ledger_reports_sth_signature_mismatch`: an STH
    /// whose recomputed Merkle root is correct but whose signature doesn't
    /// verify against the configured operator key must be reported as
    /// invalid, not silently accepted just because the root matched.
    #[test]
    fn inspect_ledger_reports_sth_signature_mismatch() {
        let entries = sample_entries(&["aa".repeat(32).as_str(), "bb".repeat(32).as_str()]);
        let hashes: Vec<String> = entries.iter().map(|e| e.entry_hash.clone()).collect();
        let root = hex::encode(avalon_chain::merkle::mth_of_hex_hashes(&hashes).unwrap());

        let signing_key = SigningKey::generate(&mut rand::rng());
        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            2,
            &root,
            "avalon-test",
            time::OffsetDateTime::UNIX_EPOCH,
        );

        // The Merkle recompute is correct on its own...
        assert!(merkle_root_matches(&entries, &sth));

        // ...but verifying against a DIFFERENT key's public half — a
        // forged/mismatched signature, or the wrong operator key configured
        // — must be reported as invalid.
        let wrong_key = SigningKey::generate(&mut rand::rng());
        assert!(
            !avalon_protocol::sth::verify_tree_head(&wrong_key.verifying_key(), &sth),
            "a signature that doesn't verify against the configured key must be reported, not accepted"
        );

        // Sanity: the actual signing key's public half verifies fine.
        assert!(avalon_protocol::sth::verify_tree_head(
            &signing_key.verifying_key(),
            &sth
        ));
    }

    #[test]
    fn merkle_root_matches_detects_a_tampered_entry() {
        let entries = sample_entries(&["aa".repeat(32).as_str(), "bb".repeat(32).as_str()]);
        let hashes: Vec<String> = entries.iter().map(|e| e.entry_hash.clone()).collect();
        let root = hex::encode(avalon_chain::merkle::mth_of_hex_hashes(&hashes).unwrap());
        let signing_key = SigningKey::generate(&mut rand::rng());
        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            2,
            &root,
            "avalon-test",
            time::OffsetDateTime::UNIX_EPOCH,
        );
        assert!(merkle_root_matches(&entries, &sth));

        let mut tampered = entries;
        tampered[0].entry_hash = "cc".repeat(32);
        assert!(
            !merkle_root_matches(&tampered, &sth),
            "a tampered entry_hash must change the recomputed root and fail the match"
        );
    }

    /// Regression test: `seq` is `GENERATED ALWAYS AS IDENTITY`, so a batch
    /// commit that rolls back after inserting rows permanently burns
    /// whatever `seq` values it allocated — a real gap, not a hypothetical
    /// one. `entries` here has three real rows at seq = 1, 2, 5 (3 and 4
    /// burned). `merkle_root_matches` must recompute the root from the
    /// first `tree_size` entries **by position**, not by filtering on
    /// `seq <= tree_size` — the latter would drop the seq=5 entry entirely
    /// for `tree_size = 3` (since 5 > 3), even though it's genuinely the
    /// third real leaf, and falsely report tamper evidence against a
    /// perfectly healthy ledger.
    #[test]
    fn merkle_root_matches_uses_position_not_seq_value_across_a_gap() {
        let mut entries = sample_entries(&[
            "aa".repeat(32).as_str(),
            "bb".repeat(32).as_str(),
            "cc".repeat(32).as_str(),
        ]);
        // sample_entries assigns dense seq = 1, 2, 3; rewrite the third
        // entry's seq to simulate seq 3 and 4 having been burned by a
        // rolled-back commit, leaving this real entry at seq = 5.
        entries[2].seq = 5;

        let hashes: Vec<String> = entries.iter().map(|e| e.entry_hash.clone()).collect();
        let root = hex::encode(avalon_chain::merkle::mth_of_hex_hashes(&hashes).unwrap());
        let signing_key = SigningKey::generate(&mut rand::rng());
        // tree_size = 3: the true leaf count (three real rows), not 5 (the
        // highest raw seq value) — matching what `commit()` now signs.
        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            3,
            &root,
            "avalon-test",
            time::OffsetDateTime::UNIX_EPOCH,
        );

        assert!(
            merkle_root_matches(&entries, &sth),
            "recompute must use the first tree_size entries by position, \
             not filter on seq <= tree_size, or it silently drops the \
             seq=5 entry and reports a false tamper-evidence mismatch"
        );
    }
}
