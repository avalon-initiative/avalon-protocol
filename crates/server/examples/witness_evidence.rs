//! Prints the witness equivocation evidence a node's schema holds for one
//! shard as a JSON array (read-only): `witness_evidence <schema> <network_id> <shard_id>`.
//! Each element carries `tree_size`, both roots, the equivocating witness key
//! ids and the cosigner key ids of each head, for the drill harness to assert on.

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(schema), Some(network_id), Some(shard_id)) = (args.next(), args.next(), args.next())
    else {
        anyhow::bail!("usage: witness_evidence <schema> <network_id> <shard_id>");
    };
    if !schema
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        anyhow::bail!("invalid schema name: {schema}");
    }

    avalon_devenv::load();
    let options = PgConnectOptions::from_str(&std::env::var("DATABASE_URL")?)?
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new().connect_with(options).await?;

    let evidence =
        avalon_chain::mirror::witness_equivocation_evidence_for(&pool, &network_id, &shard_id)
            .await?;
    let rows: Vec<serde_json::Value> = evidence
        .iter()
        .map(|e| {
            serde_json::json!({
                "tree_size": e.tree_size,
                "root_hash_a": e.head_a.sth.root_hash,
                "root_hash_b": e.head_b.sth.root_hash,
                "equivocating_witness_key_ids": e.equivocating_witness_key_ids,
                "cosigners_a": e.head_a.cosignatures.iter().map(|c| &c.witness_key_id).collect::<Vec<_>>(),
                "cosigners_b": e.head_b.cosignatures.iter().map(|c| &c.witness_key_id).collect::<Vec<_>>(),
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&rows)?);
    Ok(())
}
