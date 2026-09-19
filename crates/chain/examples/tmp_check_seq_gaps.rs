use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("connect");
    let row = sqlx::query(
        "SELECT COUNT(*) AS cnt, MAX(seq) AS max_seq, MIN(seq) AS min_seq FROM ledger_entries",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    let cnt: i64 = row.try_get("cnt").unwrap();
    let max_seq: i64 = row.try_get("max_seq").unwrap();
    let min_seq: i64 = row.try_get("min_seq").unwrap();
    println!("count={cnt} min_seq={min_seq} max_seq={max_seq} expected_count_if_no_gaps={}", max_seq - min_seq + 1);
    if cnt != max_seq - min_seq + 1 {
        println!("GAPS DETECTED: {} missing seq value(s)", (max_seq - min_seq + 1) - cnt);
    } else {
        println!("no gaps detected");
    }

    let sth_row = sqlx::query(
        "SELECT tree_size FROM signed_tree_heads ORDER BY tree_size DESC LIMIT 5",
    )
    .fetch_all(&pool)
    .await
    .expect("query failed");
    println!("recent signed_tree_heads tree_sizes:");
    for r in sth_row {
        let ts: i64 = r.try_get("tree_size").unwrap();
        println!("  {ts}");
    }
}
