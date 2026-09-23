//! Mirror-watcher storage and equivocation detection — issue #299,
//! implementing #40's decided no-consensus mirror model. See
//! `docs/projects/backend-server/architecture/settlement.md`'s equivocation-
//! detection section for the storage tables, why `detect_equivocation` is
//! a pure I/O-free function, and the durable-row-plus-structured-log
//! surfacing mechanism.
//!
//! **Issue #604: every observation, mirrored entry, and equivocation
//! finding is scoped by `shard_id`, not just `network_id`.** Every shard
//! under a `network_id` (#527/#532's sharded model) has its own
//! independent log with its own independent `seq`/`tree_size` numbering —
//! two unrelated shards both legitimately pass through `tree_size = 1`,
//! for instance. Before this, storage/verification here assumed exactly
//! one log per `network_id` (true before sharding existed), so a node
//! mirroring more than one shard of the same network got its
//! inclusion-proof verification state silently corrupted between shards,
//! and two unrelated shards reaching the same `tree_size` could even be
//! misreported as a false equivocation. `shard_id` defaults to `"core"`
//! everywhere (matching `AVALON_OWN_SHARD_ID`'s own default), so a
//! pre-sharding/single-shard deployment behaves exactly as it always did.
//! `#573` (closed) fixed the equivalent gap on the read/serving side,
//! scoped by `source_url` as a proxy for shard identity — an imperfect
//! proxy, since one peer can serve more than one shard. `source_url`
//! stays meaningful here too, but as a separate axis (which peer literally
//! served this content, for multi-peer-failover/audit purposes) — the
//! scoping/uniqueness key is `shard_id`, not `source_url`.

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

/// A shard identifier with no owning integrator (identity/social-graph/
/// guild history, #532's own routing default) — every pre-#527 deployment's
/// implicit single shard, and the default `shard_id` throughout this
/// module for exactly that reason.
pub const CORE_SHARD_ID: &str = "core";

/// One Signed Tree Head a mirror has fetched and signature-verified from
/// `source_url` — a row of `observed_sths`, or (via [`SELF_SIGNED_SOURCE`])
/// a view of this node's own [`SignedTreeHead`] history for the same
/// comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSth {
    pub source_url: String,
    pub network_id: String,
    /// Issue #604 — which shard this observation belongs to, never
    /// inferred from `source_url` (a peer can serve more than one shard).
    pub shard_id: String,
    pub tree_size: i64,
    pub root_hash: String,
    pub signature: String,
    pub signing_key_id: String,
    /// The STH's own `created_at`, as signed by its operator — distinct
    /// from `observed_at` (when *this* node happened to poll it). Issue
    /// #520: re-serving a mirrored STH later must report the value its
    /// signature actually covers, never the moment this node happened to
    /// see it, or an independent caller re-verifying the signature against
    /// the pinned network key would fail.
    pub created_at: OffsetDateTime,
    pub observed_at: OffsetDateTime,
}

impl ObservedSth {
    /// Builds the observation this node would record for `sth`, fetched
    /// from `source_url` at `observed_at` for `shard_id`. Issue #604:
    /// `shard_id` is the caller's responsibility to know — it is never
    /// derivable from `sth` itself (`SignedTreeHead` carries no shard
    /// identity of its own, only `network_id`).
    pub fn from_sth(
        source_url: impl Into<String>,
        shard_id: impl Into<String>,
        sth: &SignedTreeHead,
        observed_at: OffsetDateTime,
    ) -> Self {
        Self {
            source_url: source_url.into(),
            network_id: sth.network_id.clone(),
            shard_id: shard_id.into(),
            tree_size: sth.tree_size,
            root_hash: sth.root_hash.clone(),
            signature: sth.signature.clone(),
            signing_key_id: sth.signing_key_id.clone(),
            created_at: sth.created_at,
            observed_at,
        }
    }
}

/// The mirror's own reconstruction of the STH it originally observed —
/// issue #520, what a mirror-backed `GET /ledger/sth/*` fallback actually
/// serves. Round-trips every signed field exactly (including `created_at`,
/// per [`ObservedSth`]'s doc comment), so a caller re-verifying the
/// signature against the pinned network key gets the same bytes the
/// original operator signed, regardless of which node answered.
impl From<ObservedSth> for SignedTreeHead {
    fn from(obs: ObservedSth) -> Self {
        SignedTreeHead {
            tree_size: obs.tree_size,
            root_hash: obs.root_hash,
            network_id: obs.network_id,
            signing_key_id: obs.signing_key_id,
            signature: obs.signature,
            created_at: obs.created_at,
        }
    }
}

/// A detected equivocation: `source_a` and `source_b` both claim a
/// `root_hash` for the same `network_id`/`shard_id`/`tree_size`, and those
/// root hashes disagree — cryptographic proof that whoever signed them
/// (both observations verified against the same operator key, or this is
/// not an equivocation at all) produced two different trees at the same
/// size.
///
/// `resolved_at`/`resolved_root_hash` record a human's after-the-fact investigation: `None`
/// means still open (the mirror-watcher's equivocation gate keeps refusing
/// to backfill this network); `Some` means an operator determined which of
/// `root_hash_a`/`root_hash_b` was the legitimate tree, via
/// [`resolve_equivocation`]. Both fields are always set together — there
/// is no partial-resolution state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquivocationFinding {
    pub network_id: String,
    /// Issue #604 — which shard this finding is about; two different
    /// shards legitimately reaching the same `tree_size` is normal
    /// (every shard starts at `tree_size` 1), never itself equivocation.
    pub shard_id: String,
    pub tree_size: i64,
    pub source_a: String,
    pub root_hash_a: String,
    pub source_b: String,
    pub root_hash_b: String,
    pub resolved_at: Option<OffsetDateTime>,
    pub resolved_root_hash: Option<String>,
}

/// Compares `candidate` against every observation in `existing` at the
/// same `network_id`/`shard_id`/`tree_size` from a *different* source,
/// returning one [`EquivocationFinding`] per source whose `root_hash`
/// disagrees. Consistent observations (same `root_hash`, or a different
/// `tree_size`/`network_id`/`shard_id` entirely, or the same source
/// re-observed) never produce a finding. Pure and I/O-free on purpose —
/// see module docs.
pub fn detect_equivocation(
    existing: &[ObservedSth],
    candidate: &ObservedSth,
) -> Vec<EquivocationFinding> {
    existing
        .iter()
        .filter(|e| {
            e.network_id == candidate.network_id
                && e.shard_id == candidate.shard_id
                && e.tree_size == candidate.tree_size
                && e.source_url != candidate.source_url
                && e.root_hash != candidate.root_hash
        })
        .map(|e| EquivocationFinding {
            network_id: candidate.network_id.clone(),
            shard_id: candidate.shard_id.clone(),
            tree_size: candidate.tree_size,
            source_a: e.source_url.clone(),
            root_hash_a: e.root_hash.clone(),
            source_b: candidate.source_url.clone(),
            root_hash_b: candidate.root_hash.clone(),
            resolved_at: None,
            resolved_root_hash: None,
        })
        .collect()
}

/// Records `obs` in `observed_sths`, idempotently — re-observing the same
/// peer's STH for the same shard at the same `tree_size` on a later poll
/// tick is a no-op, not a duplicate row. Returns `true` iff this was a
/// genuinely new observation (worth running equivocation detection over);
/// `false` for a repeat.
pub async fn insert_observation(pool: &PgPool, obs: &ObservedSth) -> Result<bool, SettlementError> {
    let result = sqlx::query(
        r#"
        INSERT INTO observed_sths (source_url, network_id, shard_id, tree_size, root_hash, signature, signing_key_id, created_at, observed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (source_url, network_id, shard_id, tree_size) DO NOTHING
        "#,
    )
    .bind(&obs.source_url)
    .bind(&obs.network_id)
    .bind(&obs.shard_id)
    .bind(obs.tree_size)
    .bind(&obs.root_hash)
    .bind(&obs.signature)
    .bind(&obs.signing_key_id)
    .bind(obs.created_at)
    .bind(obs.observed_at)
    .execute(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(result.rows_affected() > 0)
}

/// Every observation (from any source, any node) at `network_id` +
/// `shard_id` + `tree_size` — what [`detect_equivocation`] compares a new
/// observation against. Does **not** include this node's own
/// [`SignedTreeHead`] history; callers that want that folded in should
/// append an [`ObservedSth::from_sth`] view of it (tagged
/// [`SELF_SIGNED_SOURCE`]) themselves, since that lives in a different
/// table (`signed_tree_heads`), not this one.
pub async fn observations_at(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    tree_size: i64,
) -> Result<Vec<ObservedSth>, SettlementError> {
    let rows = sqlx::query(
        r#"
        SELECT source_url, network_id, shard_id, tree_size, root_hash, signature, signing_key_id, created_at, observed_at
        FROM observed_sths
        WHERE network_id = $1 AND shard_id = $2 AND tree_size = $3
        "#,
    )
    .bind(network_id)
    .bind(shard_id)
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
        shard_id: row.try_get("shard_id").map_err(get)?,
        tree_size: row.try_get("tree_size").map_err(get)?,
        root_hash: row.try_get("root_hash").map_err(get)?,
        signature: row.try_get("signature").map_err(get)?,
        signing_key_id: row.try_get("signing_key_id").map_err(get)?,
        created_at: row.try_get("created_at").map_err(get)?,
        observed_at: row.try_get("observed_at").map_err(get)?,
    })
}

/// The one `observed_sths` row at `network_id`/`shard_id`/`tree_size` whose
/// `root_hash` equals `root_hash` exactly — issue #520, used to find the
/// STH that actually corresponds to a Merkle root this node just
/// recomputed from its own `mirrored_entries`, deliberately excluding any
/// other (necessarily disagreeing) observation recorded at the same
/// `network_id`/`shard_id`/`tree_size`. `None` means either no peer ever
/// reported this exact STH, or this node hasn't backfilled up to
/// `tree_size` yet. Issue #573/#604: `source_url`, when `Some`,
/// additionally scopes to one specific mirrored peer.
pub async fn observed_sth_matching_root(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    tree_size: i64,
    root_hash: &str,
    source_url: Option<&str>,
) -> Result<Option<ObservedSth>, SettlementError> {
    let mut builder: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT source_url, network_id, shard_id, tree_size, root_hash, signature, signing_key_id, \
         created_at, observed_at FROM observed_sths WHERE network_id = ",
    );
    builder.push_bind(network_id);
    builder.push(" AND shard_id = ").push_bind(shard_id);
    builder.push(" AND tree_size = ").push_bind(tree_size);
    builder.push(" AND root_hash = ").push_bind(root_hash);
    if let Some(source_url) = source_url {
        builder.push(" AND source_url = ").push_bind(source_url);
    }
    builder.push(" LIMIT 1");

    let row = builder
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
    row.map(observed_sth_from_row).transpose()
}

/// Durably records `finding` in `equivocation_findings` and emits a loud,
/// structured `tracing::error!` — see module docs for why this ticket does
/// both rather than picking one surfacing mechanism, and for why the log
/// line's fields (not just its message) are the part a hoster's own
/// alerting should key off of.
pub async fn record_equivocation(
    pool: &PgPool,
    finding: &EquivocationFinding,
) -> Result<(), SettlementError> {
    tracing::error!(
        event = "equivocation_detected",
        network_id = %finding.network_id,
        shard_id = %finding.shard_id,
        tree_size = finding.tree_size,
        source_a = %finding.source_a,
        root_hash_a = %finding.root_hash_a,
        source_b = %finding.source_b,
        root_hash_b = %finding.root_hash_b,
        "equivocation detected: the same operator has signed two different trees at the same size",
    );
    sqlx::query(
        r#"
        INSERT INTO equivocation_findings (network_id, shard_id, tree_size, source_a, root_hash_a, source_b, root_hash_b)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(&finding.network_id)
    .bind(&finding.shard_id)
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

fn equivocation_finding_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<EquivocationFinding, SettlementError> {
    let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
    Ok(EquivocationFinding {
        network_id: row.try_get("network_id").map_err(get)?,
        shard_id: row.try_get("shard_id").map_err(get)?,
        tree_size: row.try_get("tree_size").map_err(get)?,
        source_a: row.try_get("source_a").map_err(get)?,
        root_hash_a: row.try_get("root_hash_a").map_err(get)?,
        source_b: row.try_get("source_b").map_err(get)?,
        root_hash_b: row.try_get("root_hash_b").map_err(get)?,
        resolved_at: row.try_get("resolved_at").map_err(get)?,
        resolved_root_hash: row.try_get("resolved_root_hash").map_err(get)?,
    })
}

/// Every equivocation this node has ever recorded for `network_id`
/// (across every shard), newest first — resolved and unresolved alike.
/// Used for operator-facing history (`avalon list-equivocations`);
/// [`unresolved_equivocations`] is what the mirror-watcher's own backfill
/// gate checks, scoped to one shard at a time.
pub async fn list_equivocations(
    pool: &PgPool,
    network_id: &str,
) -> Result<Vec<EquivocationFinding>, SettlementError> {
    let rows = sqlx::query(
        r#"
        SELECT network_id, shard_id, tree_size, source_a, root_hash_a, source_b, root_hash_b, resolved_at, resolved_root_hash
        FROM equivocation_findings
        WHERE network_id = $1
        ORDER BY detected_at DESC
        "#,
    )
    .bind(network_id)
    .fetch_all(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;

    rows.into_iter()
        .map(equivocation_finding_from_row)
        .collect()
}

/// Every *unresolved* equivocation recorded for `network_id`/`shard_id` —
/// what the mirror-watcher's backfill gate
/// (`crate::mirror_watcher::backfill_network` in `avalon-server`) checks
/// before extending *that shard's* mirrored history. Empty means either no
/// equivocation was ever detected for this shard, or every one that was
/// has since been resolved via [`resolve_equivocation`]. Issue #604:
/// scoped per shard, not per network — an unresolved finding on one shard
/// must never block backfill of a completely different, unrelated shard
/// under the same network_id.
pub async fn unresolved_equivocations(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
) -> Result<Vec<EquivocationFinding>, SettlementError> {
    let rows = sqlx::query(
        r#"
        SELECT network_id, shard_id, tree_size, source_a, root_hash_a, source_b, root_hash_b, resolved_at, resolved_root_hash
        FROM equivocation_findings
        WHERE network_id = $1 AND shard_id = $2 AND resolved_at IS NULL
        ORDER BY detected_at DESC
        "#,
    )
    .bind(network_id)
    .bind(shard_id)
    .fetch_all(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;

    rows.into_iter()
        .map(equivocation_finding_from_row)
        .collect()
}

/// Records an operator's investigation outcome for every unresolved
/// finding at `network_id`/`shard_id`/`tree_size`: `legitimate_root_hash`
/// is whichever of that finding's two disagreeing root hashes was
/// determined genuine (per `docs/projects/backend-server/for-maintainers/equivocation-response.md`'s
/// investigation playbook). This alone does not touch `mirrored_entries` —
/// pair with [`discard_mirrored_entries_from`] to actually roll back any
/// content this node already mirrored from the losing branch before
/// backfill resumes.
///
/// Returns the number of findings marked resolved (0 if none were open at
/// this `network_id`/`shard_id`/`tree_size` — not an error, since
/// re-resolving an already-resolved finding, or one that no longer
/// exists, is a no-op rather than something worth failing a runbook step
/// over).
pub async fn resolve_equivocation(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    tree_size: i64,
    legitimate_root_hash: &str,
) -> Result<u64, SettlementError> {
    let result = sqlx::query(
        r#"
        UPDATE equivocation_findings
        SET resolved_at = now(), resolved_root_hash = $4
        WHERE network_id = $1 AND shard_id = $2 AND tree_size = $3 AND resolved_at IS NULL
        "#,
    )
    .bind(network_id)
    .bind(shard_id)
    .bind(tree_size)
    .bind(legitimate_root_hash)
    .execute(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    let rows_affected = result.rows_affected();
    tracing::info!(
        event = "equivocation_resolved",
        network_id = %network_id,
        shard_id = %shard_id,
        tree_size,
        legitimate_root_hash = %legitimate_root_hash,
        findings_resolved = rows_affected,
        "equivocation finding(s) marked resolved",
    );
    Ok(rows_affected)
}

/// Discards every `mirrored_entries` row for `network_id`/`shard_id`
/// verified against a tree at or beyond `from_tree_size` — the recovery
/// half of resolving an equivocation. Once an operator has
/// determined which branch at `from_tree_size` was legitimate
/// ([`resolve_equivocation`]), any entries this node already mirrored
/// using the *other* branch's tree must be discarded before the
/// mirror-watcher resumes backfill of that shard, or later inclusion-proof
/// verification against the now-trusted branch would be checked against
/// content it never actually produced.
///
/// Deliberately coarse rather than surgical: this drops every entry for
/// this shard verified at `verified_tree_size >= from_tree_size`,
/// including any that happened to belong to the legitimate branch, not
/// just the losing one — there is no local way to tell which of those
/// already-mirrored rows came from which branch after the fact, since both
/// were independently signature/inclusion-verified at the time. The
/// mirror-watcher's own multi-peer backfill re-fetches and
/// re-verifies everything dropped here from the now-resolved-legitimate
/// branch on its next tick — re-verification, not data loss of anything
/// the network itself considers canonical. Other shards under the same
/// `network_id` are completely unaffected.
///
/// Returns the number of rows discarded.
pub async fn discard_mirrored_entries_from(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    from_tree_size: i64,
) -> Result<u64, SettlementError> {
    let result = sqlx::query(
        "DELETE FROM mirrored_entries WHERE network_id = $1 AND shard_id = $2 AND verified_tree_size >= $3",
    )
    .bind(network_id)
    .bind(shard_id)
    .bind(from_tree_size)
    .execute(pool)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(result.rows_affected())
}

/// The highest `tree_size` this node has observed from `source_url` for
/// `network_id`/`shard_id` so far — `0` if none yet. Used to decide
/// whether a newly fetched STH is actually new before running equivocation
/// detection or a backfill pass over it.
pub async fn latest_observed_tree_size(
    pool: &PgPool,
    source_url: &str,
    network_id: &str,
    shard_id: &str,
) -> Result<i64, SettlementError> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(tree_size), 0) AS max_tree_size FROM observed_sths \
         WHERE source_url = $1 AND network_id = $2 AND shard_id = $3",
    )
    .bind(source_url)
    .bind(network_id)
    .bind(shard_id)
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
    /// Issue #604 — which shard this entry belongs to; part of this row's
    /// identity (the natural key is `network_id`/`shard_id`/`seq`), unlike
    /// `source_url`.
    pub shard_id: String,
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
/// `(network_id, shard_id, seq)`, **not** `source_url`: a re-fetch of an
/// already-mirrored entry, whether from the same peer or a different
/// configured peer of the same shard, is a no-op. This is what makes
/// multi-peer backfill safe — every configured peer of a
/// given shard is an interchangeable source of the same
/// independently-verified content, so failing over from peer A to peer B
/// mid-backfill never double-counts or restarts progress.
/// `shard_id` joined the key alongside `network_id` — two different
/// shards' entries, even at the same `seq`, are never the same row.
/// Callers must have already independently verified `entry`'s inclusion
/// against a signature-checked STH before calling this — this function
/// itself does not verify anything; see `avalon-server`'s mirror-watcher
/// for the verification step.
///
/// Generic over `sqlx::PgExecutor` (a bare `&PgPool`, or `&mut **tx` for a
/// transaction already in progress) so issue #313's local-indexer-apply
/// step can share the exact transaction this write lands in, rather than
/// committing this row and applying the projection separately.
pub async fn insert_mirrored_entry<'e, E>(
    executor: E,
    entry: &MirroredEntry,
) -> Result<(), SettlementError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        r#"
        INSERT INTO mirrored_entries
            (source_url, network_id, shard_id, seq, event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id, verified_tree_size)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        ON CONFLICT (network_id, shard_id, seq) DO NOTHING
        "#,
    )
    .bind(&entry.source_url)
    .bind(&entry.network_id)
    .bind(&entry.shard_id)
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
    .execute(executor)
    .await
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    Ok(())
}

/// How many entries this node has already verified and mirrored for
/// `network_id`/`shard_id` — across *every* configured peer of that shard,
/// not just one (multi-peer backfill: any configured peer of
/// a shard is an interchangeable source of the same verified content).
/// Both the next `since_seq` to backfill from (the highest mirrored `seq`
/// for this shard) and the leaf-index counter to hand the next entry's
/// inclusion-proof verification (a contiguous count of accepted entries
/// *for this shard*, matching how `leaf_index_for_seq`/`entry_hashes_up_to`
/// rank entries on the authority side — see
/// `crates/chain/src/postgres.rs`'s module doc comment).
///
/// Issue #604: `shard_id` is now required, not optional — every mirrored
/// entry genuinely belongs to exactly one shard, so "how far have I
/// mirrored *anyone* claiming this network" (blending unrelated shards'
/// entries into one meaningless count) was never actually the right
/// question, even before this was enforced. `source_url`, when `Some`,
/// additionally scopes this to one specific peer within that shard.
pub async fn mirrored_progress(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source_url: Option<&str>,
) -> Result<MirrorProgress, SettlementError> {
    let row = match source_url {
        Some(source_url) => sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) AS last_seq, COUNT(*) AS count FROM mirrored_entries \
             WHERE network_id = $1 AND shard_id = $2 AND source_url = $3",
        )
        .bind(network_id)
        .bind(shard_id)
        .bind(source_url)
        .fetch_one(pool)
        .await,
        None => sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) AS last_seq, COUNT(*) AS count FROM mirrored_entries \
             WHERE network_id = $1 AND shard_id = $2",
        )
        .bind(network_id)
        .bind(shard_id)
        .fetch_one(pool)
        .await,
    }
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

fn mirrored_entry_from_row(row: sqlx::postgres::PgRow) -> Result<MirroredEntry, SettlementError> {
    let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
    Ok(MirroredEntry {
        source_url: row.try_get("source_url").map_err(get)?,
        network_id: row.try_get("network_id").map_err(get)?,
        shard_id: row.try_get("shard_id").map_err(get)?,
        seq: row.try_get("seq").map_err(get)?,
        event_id: row.try_get("event_id").map_err(get)?,
        kind: row.try_get("kind").map_err(get)?,
        issuer: row.try_get("issuer").map_err(get)?,
        subject: row.try_get("subject").map_err(get)?,
        payload: row.try_get("payload").map_err(get)?,
        event_timestamp: row.try_get("event_timestamp").map_err(get)?,
        version: row.try_get("version").map_err(get)?,
        prev_hash: row.try_get("prev_hash").map_err(get)?,
        entry_hash: row.try_get("entry_hash").map_err(get)?,
        batch_id: row.try_get("batch_id").map_err(get)?,
        verified_tree_size: row.try_get("verified_tree_size").map_err(get)?,
    })
}

/// Issue #520 — mirror-side equivalent of
/// `PostgresSettlementProvider::list_entries_since_for_subject`: entries
/// for `network_id`/`shard_id` with `seq` strictly greater than
/// `since_seq`, oldest first, pre-filtered to one `subject` when `Some`.
/// What `GET /ledger/entries` falls back to once this node has no
/// locally-authored entries of its own to serve for the requested shard.
///
/// Issue #604: `shard_id` required, see [`mirrored_progress`]'s doc
/// comment for why. Issue #573: `source_url`, when `Some`, additionally
/// scopes to one specific mirrored peer within that shard.
pub async fn mirrored_entries_since(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    since_seq: i64,
    limit: i64,
    subject: Option<&str>,
    source_url: Option<&str>,
) -> Result<Vec<MirroredEntry>, SettlementError> {
    let mut builder: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT source_url, network_id, shard_id, seq, event_id, kind, issuer, subject, payload, \
         event_timestamp, version, prev_hash, entry_hash, batch_id, verified_tree_size \
         FROM mirrored_entries WHERE network_id = ",
    );
    builder.push_bind(network_id);
    builder.push(" AND shard_id = ").push_bind(shard_id);
    builder.push(" AND seq > ").push_bind(since_seq);
    if let Some(subject) = subject {
        builder.push(" AND subject = ").push_bind(subject);
    }
    if let Some(source_url) = source_url {
        builder.push(" AND source_url = ").push_bind(source_url);
    }
    builder.push(" ORDER BY seq ASC LIMIT ").push_bind(limit);

    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

    rows.into_iter().map(mirrored_entry_from_row).collect()
}

/// Issue #520 — mirror-side equivalent of
/// `PostgresSettlementProvider::entry_hashes_up_to`: the first `tree_size`
/// mirrored entries' `entry_hash` *for this shard*, oldest first.
/// Deliberately count-based (`ORDER BY seq ASC LIMIT`, not
/// `WHERE seq <= tree_size`), same rationale as the authority-side
/// version — and the same rank a mirror already assigned each entry's leaf
/// index while backfilling (`mirror_watcher::backfill`'s
/// `progress.verified_count`), so this reproduces the exact same tree an
/// inclusion proof was originally verified against.
///
/// Issue #604: `shard_id` required, see [`mirrored_progress`]'s doc
/// comment for why. Issue #573: `source_url`, when `Some`, additionally
/// scopes to one specific mirrored peer within that shard.
pub async fn mirrored_entry_hashes_up_to(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    tree_size: i64,
    source_url: Option<&str>,
) -> Result<Vec<String>, SettlementError> {
    let mut builder: sqlx::QueryBuilder<sqlx::Postgres> =
        sqlx::QueryBuilder::new("SELECT entry_hash FROM mirrored_entries WHERE network_id = ");
    builder.push_bind(network_id);
    builder.push(" AND shard_id = ").push_bind(shard_id);
    if let Some(source_url) = source_url {
        builder.push(" AND source_url = ").push_bind(source_url);
    }
    builder
        .push(" ORDER BY seq ASC LIMIT ")
        .push_bind(tree_size);

    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
    rows.into_iter()
        .map(|row| {
            row.try_get::<String, _>("entry_hash")
                .map_err(|e| SettlementError::Storage(e.to_string()))
        })
        .collect()
}

/// Issue #520 — mirror-side equivalent of
/// `PostgresSettlementProvider::leaf_index_for_seq`: the 0-indexed Merkle
/// leaf position of the mirrored entry at `seq`, among this shard's
/// mirrored entries ordered by `seq`. `None` if no mirrored entry has this
/// exact `seq` for this shard.
///
/// Issue #604: `shard_id` required, see [`mirrored_progress`]'s doc
/// comment for why. Issue #573: `source_url`, when `Some`, additionally
/// scopes to one specific mirrored peer within that shard.
pub async fn mirrored_leaf_index_for_seq(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    seq: i64,
    source_url: Option<&str>,
) -> Result<Option<i64>, SettlementError> {
    let row = match source_url {
        Some(source_url) => sqlx::query(
            r#"
            SELECT (SELECT COUNT(*) FROM mirrored_entries e2 WHERE e2.network_id = $1 AND e2.shard_id = $2 AND e2.source_url = $4 AND e2.seq <= e1.seq) - 1 AS leaf_index
            FROM mirrored_entries e1
            WHERE e1.network_id = $1 AND e1.shard_id = $2 AND e1.seq = $3 AND e1.source_url = $4
            "#,
        )
        .bind(network_id)
        .bind(shard_id)
        .bind(seq)
        .bind(source_url)
        .fetch_optional(pool)
        .await,
        None => sqlx::query(
            r#"
            SELECT (SELECT COUNT(*) FROM mirrored_entries e2 WHERE e2.network_id = $1 AND e2.shard_id = $2 AND e2.seq <= e1.seq) - 1 AS leaf_index
            FROM mirrored_entries e1
            WHERE e1.network_id = $1 AND e1.shard_id = $2 AND e1.seq = $3
            "#,
        )
        .bind(network_id)
        .bind(shard_id)
        .bind(seq)
        .fetch_optional(pool)
        .await,
    }
    .map_err(|e| SettlementError::Storage(e.to_string()))?;
    row.map(|r| {
        r.try_get("leaf_index")
            .map_err(|e| SettlementError::Storage(e.to_string()))
    })
    .transpose()
}

/// The observed STH with the highest `tree_size` for `network_id`/`shard_id`
/// (optionally from one `source_url`), if any — the furthest point a peer
/// has ever attested to, regardless of how much of it this node has
/// mirrored.
pub async fn latest_observed_sth(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source_url: Option<&str>,
) -> Result<Option<ObservedSth>, SettlementError> {
    let mut builder: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT source_url, network_id, shard_id, tree_size, root_hash, signature, signing_key_id, \
         created_at, observed_at FROM observed_sths WHERE network_id = ",
    );
    builder.push_bind(network_id);
    builder.push(" AND shard_id = ").push_bind(shard_id);
    if let Some(source_url) = source_url {
        builder.push(" AND source_url = ").push_bind(source_url);
    }
    builder.push(" ORDER BY tree_size DESC LIMIT 1");
    let row = builder
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
    row.map(observed_sth_from_row).transpose()
}

/// Every mirrored `seq` (for `network_id`/`shard_id`, optionally one
/// `source_url`) whose `prev_hash` is not the previous mirrored entry's
/// `entry_hash` — empty when the mirrored hash chain is unbroken. The first
/// mirrored entry has no predecessor in this table and is not checked.
pub async fn mirrored_chain_breaks(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source_url: Option<&str>,
) -> Result<Vec<i64>, SettlementError> {
    let mut builder: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT seq FROM (SELECT seq, prev_hash, LAG(entry_hash) OVER (ORDER BY seq) AS expected \
         FROM mirrored_entries WHERE network_id = ",
    );
    builder.push_bind(network_id);
    builder.push(" AND shard_id = ").push_bind(shard_id);
    if let Some(source_url) = source_url {
        builder.push(" AND source_url = ").push_bind(source_url);
    }
    builder.push(") t WHERE expected IS NOT NULL AND prev_hash <> expected ORDER BY seq");
    let rows = builder
        .build()
        .fetch_all(pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
    rows.into_iter()
        .map(|row| {
            row.try_get::<i64, _>("seq")
                .map_err(|e| SettlementError::Storage(e.to_string()))
        })
        .collect()
}

/// Everything [`evaluate_convergence`] needs, gathered by the caller from
/// this node's own mirror tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvergenceInputs {
    /// Number of mirrored entries, which is also the Merkle `tree_size`
    /// they cover.
    pub mirrored_count: i64,
    /// `tree_size` of the furthest observed STH, if any was ever observed.
    pub latest_observed_tree_size: Option<i64>,
    /// Whether an observed STH exists at `mirrored_count` whose root equals
    /// the root recomputed from the mirrored entries.
    pub matching_sth_at_mirrored_count: bool,
    /// Mirrored `seq`s whose `prev_hash` link is broken.
    pub chain_breaks: Vec<i64>,
    /// Count of unresolved equivocation findings for this shard.
    pub unresolved_equivocations: usize,
}

/// Whether a mirror's copy of a shard is safe to treat as the authority's
/// complete history. Only [`ConvergenceVerdict::Converged`] means yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvergenceVerdict {
    /// The mirrored entries recompute to the exact root of an observed STH
    /// at the furthest `tree_size` any peer attested to, with an unbroken
    /// hash chain and no open equivocation.
    Converged {
        /// The `tree_size` both the mirror and the newest STH cover.
        tree_size: i64,
    },
    /// Nothing has been mirrored for this shard yet.
    NothingMirrored,
    /// The mirror holds fewer entries than the furthest observed STH covers.
    Behind {
        /// Entries mirrored so far.
        mirrored: i64,
        /// `tree_size` of the furthest observed STH.
        observed: i64,
    },
    /// No STH was ever observed at all, so there is nothing to compare the
    /// mirrored data against.
    NoObservedSth,
    /// Observed STHs exist at the mirrored `tree_size`, but none has the
    /// root recomputed from the mirrored entries.
    RootMismatch {
        /// The `tree_size` at which the roots disagree.
        tree_size: i64,
    },
    /// The mirrored hash chain has broken links.
    ChainBroken {
        /// How many links are broken.
        breaks: usize,
    },
    /// An unresolved equivocation finding exists for this shard.
    BlockedByEquivocation {
        /// How many unresolved findings.
        findings: usize,
    },
}

/// Pure decision logic behind the convergence check, in order of severity:
/// open equivocation, broken chain, nothing mirrored, no observed STH,
/// lagging behind, root mismatch, and only then converged.
pub fn evaluate_convergence(inputs: &ConvergenceInputs) -> ConvergenceVerdict {
    if inputs.unresolved_equivocations > 0 {
        return ConvergenceVerdict::BlockedByEquivocation {
            findings: inputs.unresolved_equivocations,
        };
    }
    if !inputs.chain_breaks.is_empty() {
        return ConvergenceVerdict::ChainBroken {
            breaks: inputs.chain_breaks.len(),
        };
    }
    if inputs.mirrored_count == 0 {
        return ConvergenceVerdict::NothingMirrored;
    }
    let Some(observed) = inputs.latest_observed_tree_size else {
        return ConvergenceVerdict::NoObservedSth;
    };
    if observed > inputs.mirrored_count {
        return ConvergenceVerdict::Behind {
            mirrored: inputs.mirrored_count,
            observed,
        };
    }
    if inputs.matching_sth_at_mirrored_count {
        return ConvergenceVerdict::Converged {
            tree_size: inputs.mirrored_count,
        };
    }
    ConvergenceVerdict::RootMismatch {
        tree_size: inputs.mirrored_count,
    }
}

/// A convergence check's evidence and conclusion, as gathered from this
/// node's own mirror tables by [`check_convergence`].
#[derive(Debug, Clone)]
pub struct ConvergenceReport {
    /// Number of mirrored entries examined.
    pub mirrored_count: i64,
    /// Hex Merkle root recomputed over every mirrored entry, if any exist.
    pub recomputed_root: Option<String>,
    /// The furthest observed STH, if any.
    pub latest_observed: Option<ObservedSth>,
    /// Mirrored `seq`s with a broken `prev_hash` link.
    pub chain_breaks: Vec<i64>,
    /// Unresolved equivocation findings for this shard.
    pub unresolved_equivocations: usize,
    /// The conclusion drawn from the above.
    pub verdict: ConvergenceVerdict,
}

/// Gathers everything [`evaluate_convergence`] needs from this node's own
/// tables and returns it with the verdict. Needs no connection to the
/// authority, so it works while the authority is down.
pub async fn check_convergence(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
    source_url: Option<&str>,
) -> Result<ConvergenceReport, SettlementError> {
    let progress = mirrored_progress(pool, network_id, shard_id, source_url).await?;
    let hashes = mirrored_entry_hashes_up_to(
        pool,
        network_id,
        shard_id,
        progress.verified_count,
        source_url,
    )
    .await?;
    let recomputed_root = if hashes.is_empty() {
        None
    } else {
        let root = crate::merkle::mth_of_hex_hashes(&hashes)
            .map_err(|e| SettlementError::Storage(format!("invalid mirrored entry hash: {e}")))?;
        Some(hex::encode(root))
    };
    let latest_observed = latest_observed_sth(pool, network_id, shard_id, source_url).await?;
    let matching = match &recomputed_root {
        Some(root) => {
            observed_sth_matching_root(
                pool,
                network_id,
                shard_id,
                progress.verified_count,
                root,
                source_url,
            )
            .await?
        }
        None => None,
    };
    let chain_breaks = mirrored_chain_breaks(pool, network_id, shard_id, source_url).await?;
    let unresolved_equivocations = unresolved_equivocations(pool, network_id, shard_id)
        .await?
        .len();
    let verdict = evaluate_convergence(&ConvergenceInputs {
        mirrored_count: progress.verified_count,
        latest_observed_tree_size: latest_observed.as_ref().map(|sth| sth.tree_size),
        matching_sth_at_mirrored_count: matching.is_some(),
        chain_breaks: chain_breaks.clone(),
        unresolved_equivocations,
    });
    Ok(ConvergenceReport {
        mirrored_count: progress.verified_count,
        recomputed_root,
        latest_observed,
        chain_breaks,
        unresolved_equivocations,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sth(source: &str, network_id: &str, tree_size: i64, root_hash: &str) -> ObservedSth {
        sth_for_shard(source, network_id, CORE_SHARD_ID, tree_size, root_hash)
    }

    fn sth_for_shard(
        source: &str,
        network_id: &str,
        shard_id: &str,
        tree_size: i64,
        root_hash: &str,
    ) -> ObservedSth {
        ObservedSth {
            source_url: source.to_string(),
            network_id: network_id.to_string(),
            shard_id: shard_id.to_string(),
            tree_size,
            root_hash: root_hash.to_string(),
            signature: "deadbeef".to_string(),
            signing_key_id: "test-key".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
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
    fn does_not_flag_different_shards_at_the_same_tree_size() {
        // Issue #604's own acceptance case: two unrelated shards both
        // legitimately reaching tree_size=1 (which every new shard passes
        // through) must never be reported as equivocation against each
        // other.
        let existing = vec![sth_for_shard(
            "peer-a",
            "avalon-test",
            "game:ashen-realms",
            1,
            "aa".repeat(32).as_str(),
        )];
        let candidate = sth_for_shard(
            "peer-b",
            "avalon-test",
            "game:other-realm",
            1,
            "bb".repeat(32).as_str(),
        );

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

    fn converged_inputs() -> ConvergenceInputs {
        ConvergenceInputs {
            mirrored_count: 10,
            latest_observed_tree_size: Some(10),
            matching_sth_at_mirrored_count: true,
            chain_breaks: Vec::new(),
            unresolved_equivocations: 0,
        }
    }

    #[test]
    fn convergence_requires_matching_root_at_the_furthest_observed_size() {
        assert_eq!(
            evaluate_convergence(&converged_inputs()),
            ConvergenceVerdict::Converged { tree_size: 10 }
        );
    }

    #[test]
    fn a_mirror_behind_the_furthest_observed_sth_is_not_converged() {
        let inputs = ConvergenceInputs {
            latest_observed_tree_size: Some(14),
            matching_sth_at_mirrored_count: false,
            ..converged_inputs()
        };
        assert_eq!(
            evaluate_convergence(&inputs),
            ConvergenceVerdict::Behind {
                mirrored: 10,
                observed: 14
            }
        );
    }

    #[test]
    fn a_root_that_matches_no_observed_sth_is_a_mismatch() {
        let inputs = ConvergenceInputs {
            matching_sth_at_mirrored_count: false,
            ..converged_inputs()
        };
        assert_eq!(
            evaluate_convergence(&inputs),
            ConvergenceVerdict::RootMismatch { tree_size: 10 }
        );
    }

    #[test]
    fn open_equivocation_and_broken_chains_outrank_an_otherwise_matching_root() {
        let equivocating = ConvergenceInputs {
            unresolved_equivocations: 1,
            ..converged_inputs()
        };
        assert_eq!(
            evaluate_convergence(&equivocating),
            ConvergenceVerdict::BlockedByEquivocation { findings: 1 }
        );
        let broken = ConvergenceInputs {
            chain_breaks: vec![4, 5],
            ..converged_inputs()
        };
        assert_eq!(
            evaluate_convergence(&broken),
            ConvergenceVerdict::ChainBroken { breaks: 2 }
        );
    }

    #[test]
    fn an_empty_mirror_or_one_with_no_observed_sth_is_never_converged() {
        let empty = ConvergenceInputs {
            mirrored_count: 0,
            ..converged_inputs()
        };
        assert_eq!(
            evaluate_convergence(&empty),
            ConvergenceVerdict::NothingMirrored
        );
        let unobserved = ConvergenceInputs {
            latest_observed_tree_size: None,
            matching_sth_at_mirrored_count: false,
            ..converged_inputs()
        };
        assert_eq!(
            evaluate_convergence(&unobserved),
            ConvergenceVerdict::NoObservedSth
        );
    }
}
