//! Mirror-watcher storage and equivocation detection — issue #299,
//! implementing #40's decided no-consensus mirror model (see
//! `docs/architecture/settlement.md`'s "Mirror sync stays minimal"
//! section): no witness quorum, no BFT consensus. Any mirror that
//! independently observes and stores every Signed Tree Head it sees for a
//! network can be compared against another mirror's (or its own past)
//! observations at the same `tree_size`; two different `root_hash` values
//! claiming the same `tree_size` from the same operator is cryptographic
//! proof of equivocation.
//!
//! This module owns two things:
//!
//! - **Storage** for `observed_sths` (every STH a mirror has fetched and
//!   signature-verified from a peer, tagged by source) and
//!   `equivocation_findings` (a detected mismatch), plus `mirrored_entries`
//!   (verified ledger content a mirror has backfilled — see
//!   [`crate::postgres::PostgresSettlementProvider::list_entries_since`]
//!   for the read side this backfills from).
//! - **Equivocation detection** ([`detect_equivocation`]): a pure function,
//!   deliberately free of any I/O, so it's directly unit-testable without a
//!   database or network — the actual security property (`#40`'s own
//!   invariant that this ticket must "actually test, not just implement")
//!   lives here, not smeared across the async storage/HTTP plumbing that
//!   calls it.
//!
//! **Surfacing mechanism** (this ticket's own open implementation call,
//! decided here): both a durable row in `equivocation_findings` *and* an
//! `eprintln!` at what would be error level if this repo had structured
//! logging (issue #265, tracked separately as still-open work) — never
//! only one or the other. The durable row is what a human or monitoring
//! system checks after the fact / queries programmatically; the log line
//! is what an operator watching the process's own output sees the moment
//! it happens. Belt and suspenders, matching how `outbox::drain_once`
//! already handles its own "this must never fail silently forever" case.

use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use crate::sth::SignedTreeHead;
use crate::SettlementError;

/// `source_url` used for this node's own signed history
/// (`signed_tree_heads`) when it's folded into an equivocation check
/// alongside observations fetched from real peers — a node that is both a
/// Settlement authority and a mirror-watcher must catch itself disagreeing
/// with what it broadcasts just as readily as it catches two peers
/// disagreeing with each other.
pub const SELF_SIGNED_SOURCE: &str = "self:signed-history";

/// One Signed Tree Head a mirror has fetched and signature-verified from
/// `source_url` — a row of `observed_sths`, or (via [`SELF_SIGNED_SOURCE`])
/// a view of this node's own [`SignedTreeHead`] history for the same
/// comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSth {
    pub source_url: String,
    pub network_id: String,
    pub tree_size: i64,
    pub root_hash: String,
    pub signature: String,
    pub signing_key_id: String,
    pub observed_at: OffsetDateTime,
}

impl ObservedSth {
    /// Builds the observation this node would record for `sth`, fetched
    /// from `source_url` at `observed_at`.
    pub fn from_sth(
        source_url: impl Into<String>,
        sth: &SignedTreeHead,
        observed_at: OffsetDateTime,
    ) -> Self {
        Self {
            source_url: source_url.into(),
            network_id: sth.network_id.clone(),
            tree_size: sth.tree_size,
            root_hash: sth.root_hash.clone(),
            signature: sth.signature.clone(),
            signing_key_id: sth.signing_key_id.clone(),
            observed_at,
        }
    }
}

/// A detected equivocation: `source_a` and `source_b` both claim a
/// `root_hash` for the same `network_id`/`tree_size`, and those root
/// hashes disagree — cryptographic proof that whoever signed them (both
/// observations verified against the same operator key, or this is not an
/// equivocation at all) produced two different trees at the same size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquivocationFinding {
    pub network_id: String,
    pub tree_size: i64,
    pub source_a: String,
    pub root_hash_a: String,
    pub source_b: String,
    pub root_hash_b: String,
}

/// Compares `candidate` against every observation in `existing` at the
/// same `network_id`/`tree_size` from a *different* source, returning one
/// [`EquivocationFinding`] per source whose `root_hash` disagrees.
/// Consistent observations (same `root_hash`, or a different `tree_size`/
/// `network_id` entirely, or the same source re-observed) never produce a
/// finding. Pure and I/O-free on purpose — see module docs.
pub fn detect_equivocation(
    existing: &[ObservedSth],
    candidate: &ObservedSth,
) -> Vec<EquivocationFinding> {
    existing
        .iter()
        .filter(|e| {
            e.network_id == candidate.network_id
                && e.tree_size == candidate.tree_size
                && e.source_url != candidate.source_url
                && e.root_hash != candidate.root_hash
        })
        .map(|e| EquivocationFinding {
            network_id: candidate.network_id.clone(),
            tree_size: candidate.tree_size,
            source_a: e.source_url.clone(),
            root_hash_a: e.root_hash.clone(),
            source_b: candidate.source_url.clone(),
            root_hash_b: candidate.root_hash.clone(),
        })
        .collect()
}

/// Records `obs` in `observed_sths`, idempotently — re-observing the same
/// peer's STH at the same `tree_size` on a later poll tick is a no-op, not
/// a duplicate row. Returns `true` iff this was a genuinely new
/// observation (worth running equivocation detection over); `false` for a
/// repeat.
pub async fn insert_observation(pool: &PgPool, obs: &ObservedSth) -> Result<bool, SettlementError> {
    let result = sqlx::query(
        r#"
        INSERT INTO observed_sths (source_url, network_id, tree_size, root_hash, signature, signing_key_id, observed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (source_url, network_id, tree_size) DO NOTHING
        "#,
    )
    .bind(&obs.source_url)
    .bind(&obs.network_id)
    .bind(obs.tree_size)
    .bind(&obs.root_hash)
    .bind(&obs.signature)
    .bind(&obs.signing_key_id)
    .bind(obs.observed_at)
    .execute(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(result.rows_affected() > 0)
}

/// Every observation (from any source, any node) at `network_id` +
/// `tree_size` — what [`detect_equivocation`] compares a new observation
/// against. Does **not** include this node's own [`SignedTreeHead`]
/// history; callers that want that folded in should append an
/// [`ObservedSth::from_sth`] view of it (tagged [`SELF_SIGNED_SOURCE`])
/// themselves, since that lives in a different table
/// (`signed_tree_heads`), not this one.
pub async fn observations_at(
    pool: &PgPool,
    network_id: &str,
    tree_size: i64,
) -> Result<Vec<ObservedSth>, SettlementError> {
    let rows = sqlx::query(
        r#"
        SELECT source_url, network_id, tree_size, root_hash, signature, signing_key_id, observed_at
        FROM observed_sths
        WHERE network_id = $1 AND tree_size = $2
        "#,
    )
    .bind(network_id)
    .bind(tree_size)
    .fetch_all(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;

    rows.into_iter().map(observed_sth_from_row).collect()
}

fn observed_sth_from_row(row: sqlx::postgres::PgRow) -> Result<ObservedSth, SettlementError> {
    let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
    Ok(ObservedSth {
        source_url: row.try_get("source_url").map_err(get)?,
        network_id: row.try_get("network_id").map_err(get)?,
        tree_size: row.try_get("tree_size").map_err(get)?,
        root_hash: row.try_get("root_hash").map_err(get)?,
        signature: row.try_get("signature").map_err(get)?,
        signing_key_id: row.try_get("signing_key_id").map_err(get)?,
        observed_at: row.try_get("observed_at").map_err(get)?,
    })
}

/// Durably records `finding` in `equivocation_findings` and emits a loud,
/// error-level-equivalent log line — see module docs for why this ticket
/// does both rather than picking one surfacing mechanism.
pub async fn record_equivocation(
    pool: &PgPool,
    finding: &EquivocationFinding,
) -> Result<(), SettlementError> {
    eprintln!(
        "EQUIVOCATION DETECTED: network_id={} tree_size={} source_a={} root_hash_a={} source_b={} root_hash_b={} — the same operator has signed two different trees at the same size",
        finding.network_id,
        finding.tree_size,
        finding.source_a,
        finding.root_hash_a,
        finding.source_b,
        finding.root_hash_b,
    );
    sqlx::query(
        r#"
        INSERT INTO equivocation_findings (network_id, tree_size, source_a, root_hash_a, source_b, root_hash_b)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(&finding.network_id)
    .bind(finding.tree_size)
    .bind(&finding.source_a)
    .bind(&finding.root_hash_a)
    .bind(&finding.source_b)
    .bind(&finding.root_hash_b)
    .execute(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(())
}

/// Every equivocation this node has ever recorded for `network_id`, newest
/// first.
pub async fn list_equivocations(
    pool: &PgPool,
    network_id: &str,
) -> Result<Vec<EquivocationFinding>, SettlementError> {
    let rows = sqlx::query(
        r#"
        SELECT network_id, tree_size, source_a, root_hash_a, source_b, root_hash_b
        FROM equivocation_findings
        WHERE network_id = $1
        ORDER BY detected_at DESC
        "#,
    )
    .bind(network_id)
    .fetch_all(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;

    let mut findings = Vec::with_capacity(rows.len());
    for row in rows {
        let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
        findings.push(EquivocationFinding {
            network_id: row.try_get("network_id").map_err(get)?,
            tree_size: row.try_get("tree_size").map_err(get)?,
            source_a: row.try_get("source_a").map_err(get)?,
            root_hash_a: row.try_get("root_hash_a").map_err(get)?,
            source_b: row.try_get("source_b").map_err(get)?,
            root_hash_b: row.try_get("root_hash_b").map_err(get)?,
        });
    }
    Ok(findings)
}

/// The highest `tree_size` this node has observed from `source_url` for
/// `network_id` so far — `0` if none yet. Used to decide whether a newly
/// fetched STH is actually new before running equivocation detection or a
/// backfill pass over it.
pub async fn latest_observed_tree_size(
    pool: &PgPool,
    source_url: &str,
    network_id: &str,
) -> Result<i64, SettlementError> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(tree_size), 0) AS max_tree_size FROM observed_sths WHERE source_url = $1 AND network_id = $2",
    )
    .bind(source_url)
    .bind(network_id)
    .fetch_one(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    row.try_get("max_tree_size")
        .map_err(|e| SettlementError::Storage(e.to_string()))
}

/// One verified ledger entry a mirror has backfilled — what
/// [`insert_mirrored_entry`] writes, built from
/// [`crate::postgres::LedgerEntryView`] plus which `tree_size` its
/// inclusion was verified against. `source_url` is an audit trail of which
/// configured peer this particular entry happened to be fetched from, not
/// part of the row's identity — see [`insert_mirrored_entry`]'s doc
/// comment.
pub struct MirroredEntry {
    pub source_url: String,
    pub network_id: String,
    pub seq: i64,
    pub event_id: uuid::Uuid,
    pub kind: String,
    pub issuer: String,
    pub subject: String,
    pub payload: Option<serde_json::Value>,
    pub event_timestamp: OffsetDateTime,
    pub version: i32,
    pub prev_hash: String,
    pub entry_hash: String,
    pub batch_id: uuid::Uuid,
    pub verified_tree_size: i64,
}

/// Writes `entry` into `mirrored_entries`, idempotently — keyed on
/// `(network_id, seq)`, **not** `source_url`: a re-fetch of an
/// already-mirrored entry, whether from the same peer or a different
/// configured peer of the same network, is a no-op. This is what makes
/// multi-peer backfill (issue #299) safe — every configured peer of a
/// network is an interchangeable source of the same independently-verified
/// content, so failing over from peer A to peer B mid-backfill never
/// double-counts or restarts progress. Callers must have already
/// independently verified `entry`'s inclusion against a signature-checked
/// STH before calling this — this function itself does not verify
/// anything; see `avalon-server`'s mirror-watcher for the verification
/// step.
pub async fn insert_mirrored_entry(
    pool: &PgPool,
    entry: &MirroredEntry,
) -> Result<(), SettlementError> {
    sqlx::query(
        r#"
        INSERT INTO mirrored_entries
            (source_url, network_id, seq, event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id, verified_tree_size)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        ON CONFLICT (network_id, seq) DO NOTHING
        "#,
    )
    .bind(&entry.source_url)
    .bind(&entry.network_id)
    .bind(entry.seq)
    .bind(entry.event_id)
    .bind(&entry.kind)
    .bind(&entry.issuer)
    .bind(&entry.subject)
    .bind(&entry.payload)
    .bind(entry.event_timestamp)
    .bind(entry.version)
    .bind(&entry.prev_hash)
    .bind(&entry.entry_hash)
    .bind(entry.batch_id)
    .bind(entry.verified_tree_size)
    .execute(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(())
}

/// How many entries this node has already verified and mirrored for
/// `network_id` — across *every* configured peer of that network, not just
/// one (issue #299's multi-peer backfill: any configured peer is an
/// interchangeable source of the same verified content). Both the next
/// `since_seq` to backfill from (the highest mirrored `seq`) and the
/// leaf-index counter to hand the next entry's inclusion-proof
/// verification (a contiguous count of accepted entries, matching how
/// `leaf_index_for_seq`/`entry_hashes_up_to` rank entries on the authority
/// side — see `crates/chain/src/postgres.rs`'s module doc comment).
pub async fn mirrored_progress(
    pool: &PgPool,
    network_id: &str,
) -> Result<MirrorProgress, SettlementError> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(seq), 0) AS last_seq, COUNT(*) AS count FROM mirrored_entries WHERE network_id = $1",
    )
    .bind(network_id)
    .fetch_one(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(MirrorProgress {
        last_seq: row
            .try_get("last_seq")
            .map_err(|e| SettlementError::Storage(e.to_string()))?,
        verified_count: row
            .try_get("count")
            .map_err(|e| SettlementError::Storage(e.to_string()))?,
    })
}

pub struct MirrorProgress {
    pub last_seq: i64,
    pub verified_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sth(source: &str, network_id: &str, tree_size: i64, root_hash: &str) -> ObservedSth {
        ObservedSth {
            source_url: source.to_string(),
            network_id: network_id.to_string(),
            tree_size,
            root_hash: root_hash.to_string(),
            signature: "deadbeef".to_string(),
            signing_key_id: "test-key".to_string(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn flags_two_different_root_hashes_at_the_same_network_and_tree_size() {
        let existing = vec![sth("peer-a", "avalon-test", 10, "aa".repeat(32).as_str())];
        let candidate = sth("peer-b", "avalon-test", 10, "bb".repeat(32).as_str());

        let findings = detect_equivocation(&existing, &candidate);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_a, "peer-a");
        assert_eq!(findings[0].source_b, "peer-b");
        assert_eq!(findings[0].tree_size, 10);
        assert_eq!(findings[0].network_id, "avalon-test");
    }

    #[test]
    fn does_not_flag_consistent_observations() {
        let root = "cc".repeat(32);
        let existing = vec![sth("peer-a", "avalon-test", 10, &root)];
        let candidate = sth("peer-b", "avalon-test", 10, &root);

        assert!(detect_equivocation(&existing, &candidate).is_empty());
    }

    #[test]
    fn does_not_flag_different_tree_sizes_or_networks() {
        let existing = vec![
            sth("peer-a", "avalon-test", 9, "aa".repeat(32).as_str()),
            sth("peer-a", "avalon-other", 10, "bb".repeat(32).as_str()),
        ];
        let candidate = sth("peer-b", "avalon-test", 10, "cc".repeat(32).as_str());

        assert!(detect_equivocation(&existing, &candidate).is_empty());
    }

    #[test]
    fn does_not_flag_the_same_source_re_observing_itself() {
        let existing = vec![sth("peer-a", "avalon-test", 10, "aa".repeat(32).as_str())];
        // Same source, different root hash — e.g. a stale re-fetch racing a
        // fresh one. Not equivocation on its own; a single source can't
        // equivocate against itself in one observation stream, only two
        // distinct sources disagreeing counts.
        let candidate = sth("peer-a", "avalon-test", 10, "bb".repeat(32).as_str());

        assert!(detect_equivocation(&existing, &candidate).is_empty());
    }

    #[test]
    fn flags_disagreement_with_this_nodes_own_signed_history() {
        let existing = vec![sth(
            SELF_SIGNED_SOURCE,
            "avalon-test",
            5,
            "aa".repeat(32).as_str(),
        )];
        let candidate = sth("peer-a", "avalon-test", 5, "bb".repeat(32).as_str());

        let findings = detect_equivocation(&existing, &candidate);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_a, SELF_SIGNED_SOURCE);
    }

    #[test]
    fn flags_against_every_disagreeing_source_not_just_the_first() {
        let existing = vec![
            sth("peer-a", "avalon-test", 10, "aa".repeat(32).as_str()),
            sth("peer-b", "avalon-test", 10, "bb".repeat(32).as_str()),
            sth("peer-c", "avalon-test", 10, "cc".repeat(32).as_str()),
        ];
        let candidate = sth("peer-d", "avalon-test", 10, "dd".repeat(32).as_str());

        let findings = detect_equivocation(&existing, &candidate);
        assert_eq!(findings.len(), 3);
    }
}
