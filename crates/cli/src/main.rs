//! `avalon` — local dev/ops CLI.
//!
//! Always available, in any build: `avalon inspect-ledger`,
//! `avalon inspect-ledger-full` (same view, plus each entry's payload),
//! `avalon outbox-status` — read-only diagnostics, safe against any
//! deployment including a real one. `inspect-ledger`/`-full` read from
//! Postgres by default, or from a RocksDB path with `--rocksdb <path>` when
//! this binary is built with the (off-by-default) `rocksdb-backend`
//! feature — see `avalon_chain::RocksDbSettlementProvider` (issue #178).
//! Nothing in `avalon-server` actually writes to RocksDB yet; this is
//! inspection-only wiring for whichever store you point it at.
//!
//! Available only when this binary is built with the default `dev-tools`
//! Cargo feature (issue #173 — see `dev_tools.rs`'s own doc comment for the
//! full rationale): `avalon create-identity`, `avalon login <identity_id>`
//! (issue #115), `avalon register-game` (issue #29). A build compiled with
//! `--no-default-features` doesn't merely refuse these commands at
//! runtime — they, and every dependency only they need, are absent from the
//! binary entirely. `issue-achievement` (per `docs/Proposal.md` §23's
//! milestone-1 vertical slice) isn't wired up yet either way — it depends
//! on the Achievements epic, still unbuilt.

#[cfg(feature = "dev-tools")]
mod dev_tools;

use avalon_chain::PostgresSettlementProvider;
use sqlx::postgres::PgPoolOptions;
use std::path::PathBuf;
use uuid::Uuid;

/// Pulls a trailing `--rocksdb <path>` flag off `args`, if present — the
/// only extra flag `inspect-ledger`/`inspect-ledger-full` accept. Returns
/// `None` (meaning "use Postgres, via `DATABASE_URL`") for everything else.
fn parse_rocksdb_flag(args: &mut std::env::Args) -> Option<PathBuf> {
    match args.next() {
        Some(flag) if flag == "--rocksdb" => {
            let Some(path) = args.next() else {
                eprintln!("--rocksdb requires a path");
                std::process::exit(1);
            };
            Some(PathBuf::from(path))
        }
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let mut args = std::env::args();
    let command = args.nth(1);
    match command.as_deref() {
        Some("inspect-ledger") => {
            let rocksdb_path = parse_rocksdb_flag(&mut args);
            inspect_ledger(false, rocksdb_path).await
        }
        Some("inspect-ledger-full") => {
            let rocksdb_path = parse_rocksdb_flag(&mut args);
            inspect_ledger(true, rocksdb_path).await
        }
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
        Some("outbox-status") => outbox_status().await,
        #[cfg(feature = "dev-tools")]
        Some("register-game") => {
            let raw_args: Vec<String> = args.collect();
            match dev_tools::RegisterGameArgs::parse(&raw_args) {
                Ok(parsed) => dev_tools::register_game(parsed).await,
                Err(message) => {
                    eprintln!("{message}");
                    eprintln!("{}", dev_tools::REGISTER_GAME_USAGE);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!(
                "usage: avalon <inspect-ledger [--rocksdb <path>]|inspect-ledger-full [--rocksdb <path>]|outbox-status{}>",
                if cfg!(feature = "dev-tools") {
                    "|create-identity|login <identity_id>|register-game --slug <slug> --name <name> --developer <dev> [--capability <cap>]... [--server <url>]"
                } else {
                    ""
                }
            );
            std::process::exit(1);
        }
    }
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

/// `full: false` is `avalon inspect-ledger` — the concise chain-integrity
/// view. `full: true` is `avalon inspect-ledger-full` — the same view plus
/// each entry's actual payload (pretty-printed JSON) and version, for
/// answering "what's actually inside this block" rather than just "is the
/// chain intact." Same query either way (`list_entries` always fetches the
/// payload, since it needs it to re-verify each entry's hash) — this only
/// changes what gets printed.
///
/// `rocksdb_path`, if given, reads from a `RocksDbSettlementProvider` at
/// that path instead of Postgres (issue #178) — requires this binary to be
/// built with `--features rocksdb-backend`. Either way the actual printing
/// (`print_ledger`) is identical: both backends return the same
/// `LedgerEntryView`/`LedgerBatchView` types, so there's exactly one
/// rendering to keep correct.
async fn inspect_ledger(full: bool, rocksdb_path: Option<PathBuf>) {
    if let Some(path) = rocksdb_path {
        return inspect_ledger_rocksdb(full, &path);
    }

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
    print_ledger(&entries, &batches, full);
}

/// The RocksDB half of `inspect_ledger` — only compiled in when this binary
/// is built with `--features rocksdb-backend` (issue #178); otherwise
/// `--rocksdb` fails with a clear message instead of silently doing
/// nothing. Synchronous (unlike the Postgres path): `RocksDbSettlementProvider`'s
/// own inspection methods are plain local-disk reads, not network I/O.
#[cfg(feature = "rocksdb-backend")]
fn inspect_ledger_rocksdb(full: bool, path: &std::path::Path) {
    let chain = avalon_chain::RocksDbSettlementProvider::open_existing(path).unwrap_or_else(|e| {
        eprintln!("failed to open {path:?}: {e}");
        std::process::exit(1);
    });
    println!("network_id: {}", chain.network_id());

    let entries = chain.list_entries().expect("failed to read ledger");
    let batches = chain.list_batches().expect("failed to read batches");
    print_ledger(&entries, &batches, full);
}

#[cfg(not(feature = "rocksdb-backend"))]
fn inspect_ledger_rocksdb(_full: bool, _path: &std::path::Path) {
    eprintln!("--rocksdb requires avalon-cli built with --features rocksdb-backend");
    std::process::exit(1);
}

/// Prints a batch boundary header (issue #38) whenever the entry stream
/// crosses into a new `batch_id`, showing that batch's seq range and root —
/// entries within a batch stay hash-chained exactly as before, this only
/// adds where the batch lines are drawn. Shared by both backends (issue
/// #178) — `LedgerEntryView`/`LedgerBatchView` are backend-agnostic, so
/// there's exactly one rendering to keep correct rather than two that could
/// drift.
fn print_ledger(
    entries: &[avalon_chain::LedgerEntryView],
    batches: &[avalon_chain::LedgerBatchView],
    full: bool,
) {
    if entries.is_empty() {
        println!("(ledger is empty)");
        return;
    }

    let batch_by_id: std::collections::HashMap<Uuid, &avalon_chain::LedgerBatchView> =
        batches.iter().map(|b| (b.batch_id, b)).collect();

    let mut current_batch: Option<Uuid> = None;
    for entry in entries {
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

        let verified = if entry.chain_intact {
            "✓"
        } else {
            "✗ BROKEN CHAIN"
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
            let pretty = serde_json::to_string_pretty(&entry.payload)
                .unwrap_or_else(|_| entry.payload.to_string());
            println!("│ payload:");
            for line in pretty.lines() {
                println!("│   {line}");
            }
        }
        println!("│ hash:      {}", short_hash(&entry.entry_hash));
        println!("│ prev:      {}", short_hash(&entry.prev_hash));
        println!("│ verified:  {verified}");
        println!("└──────────────────────────────────────────────────────");
    }

    let broken = entries.iter().filter(|e| !e.chain_intact).count();
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
}

fn short_hash(hash: &str) -> String {
    if hash.len() <= 16 {
        hash.to_string()
    } else {
        format!("{}...{}", &hash[..8], &hash[hash.len() - 8..])
    }
}
