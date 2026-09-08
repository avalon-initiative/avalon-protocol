//! `avalon` — local dev/ops CLI.
//!
//! Commands: `avalon inspect-ledger`. `register-game` and `issue-achievement`
//! (per `docs/Proposal.md` §23's milestone-1 vertical slice) aren't wired up yet — they
//! depend on the Game Registration and Achievements epics, still unbuilt.

use avalon_chain::PostgresSettlementProvider;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("inspect-ledger") => inspect_ledger().await,
        _ => {
            eprintln!("usage: avalon inspect-ledger");
            std::process::exit(1);
        }
    }
}

async fn inspect_ledger() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    let chain = PostgresSettlementProvider::new(pool);
    let entries = chain.list_entries().await.expect("failed to read ledger");

    if entries.is_empty() {
        println!("(ledger is empty)");
        return;
    }

    for entry in &entries {
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
        println!("│ hash:      {}", short_hash(&entry.entry_hash));
        println!("│ prev:      {}", short_hash(&entry.prev_hash));
        println!("│ verified:  {verified}");
        println!("└──────────────────────────────────────────────────────");
    }

    let broken = entries.iter().filter(|e| !e.chain_intact).count();
    println!();
    println!(
        "{} entries, {}",
        entries.len(),
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
