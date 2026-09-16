//! `avalon` — local dev/ops CLI.
//!
//! Always available, in any build: `avalon inspect-ledger`,
//! `avalon inspect-ledger-full` (same view, plus each entry's payload),
//! `avalon outbox-status` — read-only diagnostics, safe against any
//! deployment including a real one. `avalon prune-ledger` (issue #208) is
//! the operator-facing entry point for node-tiered retention pruning — see
//! `avalon_chain::retention`'s module doc comment for the full design;
//! this command itself only reads `AVALON_RETENTION_*` from the
//! environment and reports/executes exactly what that config says, nothing
//! more.
//!
//! Available only when this binary is built with the default `dev-tools`
//! Cargo feature (issue #173 — see `dev_tools.rs`'s own doc comment for the
//! full rationale): `avalon create-identity`, `avalon login <identity_id>`
//! (issue #115), `avalon register-integrator` (issue #29, renamed from
//! `register-game` by #290 — the old name stays a working alias), and
//! `avalon pair-device`
//! (issue #307 — drives the `start`/`poll` side of cross-device pairing,
//! standing in for a real WebAuthn-incapable client so that flow is
//! testable without a real console/engine), `avalon issue-achievement`
//! (issue #48 — issues an already-defined achievement to the identity
//! behind `--token`, through `avalon-sdk`, resolving the issuer's
//! signing key/key id from what `register-integrator` saved unless
//! overridden), and `avalon register-issuer` (issue #483, on top of
//! #481's endpoint — registers an integrator's key as an issuer on an
//! explicitly declared target network, `--network-id <id>` or
//! `--env <dev|int|mainnet>`, refusing client-side on a mismatch against
//! the server's independently STH-verified network rather than trusting
//! its bare `network_id`). A build compiled with `--no-default-features` doesn't
//! merely refuse these commands at runtime — they, and every dependency
//! only they need, are absent from the binary entirely.

#[cfg(feature = "dev-tools")]
mod dev_tools;

use avalon_chain::PostgresSettlementProvider;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

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
        Some("list-equivocations") => {
            list_equivocations(args.next()).await;
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
                "usage: avalon <inspect-ledger|inspect-ledger-full|outbox-status|prune-ledger [--dry-run]|rebuild-index|list-equivocations [network_id]|resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--discard-mirrored]{}>",
                if cfg!(feature = "dev-tools") {
                    "|create-identity|login <identity_id>|register-integrator|register-game --slug <slug> --name <name> --owner-name <owner> [--capability <cap>]... [--server <url>]|issue-achievement --integrator <slug> --achievement <key> --token <session-token> [--key <path>] [--key-id <uuid>] [--server <url>]|register-issuer --integrator <slug> (--network-id <network_id> | --env <dev|int|mainnet>) [--issuer-ref <ref>] [--key <path>] [--server <url>]|pair-device"
                } else {
                    ""
                }
            );
            std::process::exit(1);
        }
    }
}

/// `register-integrator` is the primary command name (#290); `register-game`
/// (the original, #29) stays a working deprecated alias so existing scripts
/// keep running. Both route here to the exact same
/// `RegisterIntegratorArgs::parse`/`register_integrator` call — see
/// `docs/architecture/issuers.md`.
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
            "┌─ tree_size {} ─────────────────────────────────────",
            f.tree_size
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
                 past this point until this is resolved (see docs/maintainers/equivocation-response.md)"
            ),
        }
        println!("└──────────────────────────────────────────────────────");
    }
}

/// `avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--discard-mirrored]`
/// — issue #316, the write side of #300's decided equivocation response
/// procedure. Records that an operator has completed the investigation
/// playbook (`docs/maintainers/equivocation-response.md`) and determined
/// which of the two disagreeing root hashes at `tree_size` was legitimate.
///
/// `--discard-mirrored` additionally drops any `mirrored_entries` this node
/// already verified at or beyond `tree_size` (`avalon_chain::mirror::
/// discard_mirrored_entries_from`) — pass it when this node's own mirrored
/// history might include content from the losing branch, so the next
/// mirror-watcher tick re-fetches and re-verifies from a clean point rather
/// than resuming on top of potentially-wrong local state. Safe to omit (and
/// re-run with it later) if unsure; it is not the default because a pure
/// mirror that never actually advanced past `tree_size` has nothing to
/// discard, and unconditionally deleting is needless churn for that
/// (expected to be the more common) case.
async fn resolve_equivocation(raw_args: &[String]) {
    let discard_mirrored = raw_args.iter().any(|a| a == "--discard-mirrored");
    let positional: Vec<&String> = raw_args.iter().filter(|a| !a.starts_with("--")).collect();
    let [network_id, tree_size_raw, legitimate_root_hash] = positional[..] else {
        eprintln!(
            "usage: avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--discard-mirrored]"
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
        tree_size,
        legitimate_root_hash,
    )
    .await
    .expect("failed to resolve equivocation finding(s)");
    if resolved_count == 0 {
        println!(
            "no unresolved finding at network_id={network_id} tree_size={tree_size} — nothing to do \
             (already resolved, or never existed)"
        );
        return;
    }
    println!(
        "resolved {resolved_count} finding(s) at network_id={network_id} tree_size={tree_size}: \
         legitimate root is {}",
        short_hash(legitimate_root_hash)
    );

    if discard_mirrored {
        let discarded =
            avalon_chain::mirror::discard_mirrored_entries_from(&pool, network_id, tree_size)
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

/// `full: false` is `avalon inspect-ledger` — the concise chain-integrity
/// view. `full: true` is `avalon inspect-ledger-full` — the same view plus
/// each entry's actual payload (pretty-printed JSON) and version, for
/// answering "what's actually inside this block" rather than just "is the
/// chain intact." Same query either way (`list_entries` always fetches the
/// payload, since it needs it to re-verify each entry's hash) — this only
/// changes what gets printed.
///
/// Prints a batch boundary header (issue #38) whenever the entry stream
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

    // Read-only: reports whatever genesis is already there (issue #173)
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
            "retention: hot-tier / pruned — {pruned} of {} entries have had their payload pruned locally (still fully present in every other node/mirror the network guarantees, or, at milestone-1 scale with one settlement database, permanently gone — see docs/architecture/nodes.md)",
            entries.len()
        );
    }

    // Signed Tree Head verification (issue #210) — a second, independent
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
            match avalon_chain::sth::load_verify_key_from_env() {
                Ok(verify_key) => {
                    let signature_valid = avalon_chain::sth::verify_tree_head(&verify_key, sth);
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
/// (`avalon_chain::sth::verify_tree_head`). Pure and directly unit-testable
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
    sth: &avalon_chain::sth::SignedTreeHead,
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
    use avalon_chain::sth::sign_tree_head;
    use avalon_chain::LedgerEntryView;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    #[cfg(feature = "dev-tools")]
    #[test]
    fn register_game_stays_a_working_alias_for_register_integrator() {
        assert!(is_register_integrator_command("register-integrator"));
        assert!(is_register_integrator_command("register-game"));
        assert!(!is_register_integrator_command("register-app"));
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
            !avalon_chain::sth::verify_tree_head(&wrong_key.verifying_key(), &sth),
            "a signature that doesn't verify against the configured key must be reported, not accepted"
        );

        // Sanity: the actual signing key's public half verifies fine.
        assert!(avalon_chain::sth::verify_tree_head(
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
