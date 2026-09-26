//! Prints the stored equivocation evidence for a shard, one JSON object per
//! row: `witness_evidence <network_id> <shard_id>`, reading `DATABASE_URL`
//! (used by `scripts/witness-drill.sh`).

use avalon_chain::mirror::{witness_equivocation_evidence_for, EquivocationEvidenceKind};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(network_id), Some(shard_id)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: witness_evidence <network_id> <shard_id>");
    };
    avalon_devenv::load();
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    for row in witness_equivocation_evidence_for(&pool, &network_id, &shard_id).await? {
        let kind = match row.kind {
            EquivocationEvidenceKind::Author => "author",
            EquivocationEvidenceKind::Witness => "witness",
        };
        println!(
            "{}",
            serde_json::json!({
                "kind": kind,
                "tree_size": row.tree_size,
                "root_hash_a": row.head_a.sth.root_hash,
                "root_hash_b": row.head_b.sth.root_hash,
                "equivocating_witnesses": row.equivocating_witness_key_ids,
            })
        );
    }
    Ok(())
}
