//! Node-tiered durable history retention — issue #208, implementing #180's
//! decision (see `docs/architecture/nodes.md`'s "Settlement retention
//! tiers" section for the decided shape).
//!
//! **Settlement commitment vs. durable event storage are separate
//! retention problems.** The hash-chained/Merkle-committed log itself
//! (`ledger_entries.entry_hash`/`prev_hash`/`seq`, `ledger_batches`,
//! `signed_tree_heads`) is never affected by anything in this module — it
//! stays small and permanent on every node regardless of retention tier.
//! What this module tiers is the raw signed event *body*
//! (`ledger_entries.payload`), which is what actually grows without bound
//! (see `docs/architecture/scalability.md`'s ~3.6 TB/year number).
//!
//! Two tiers, per #180:
//!
//! - **Full / archive** — retains every payload, forever. The default —
//!   pruning is opt-in, never assumed.
//! - **Hot** — retains payloads only for a configurable recent window
//!   (`AVALON_RETENTION_HOT_WINDOW_DAYS`), *if* pruning is separately and
//!   explicitly enabled (`AVALON_RETENTION_PRUNING_ENABLED=true`). A
//!   hot-tier node with pruning left disabled behaves exactly like a full
//!   node — it just declares an intent to prune later, once the operator
//!   turns the flag on.
//!
//! **This milestone-1 reality check matters and is not glossed over**: per
//! `docs/architecture/nodes.md` and `docs/architecture/settlement.md`,
//! there is currently exactly one settlement node/database. #180's
//! invariant — a hot-tier node may only discard payloads the network still
//! guarantees availability for elsewhere (an archive-tier mirror, or a
//! minimum archive-replication factor) — has no real multi-node mirror
//! network to be gated against yet. Enabling `AVALON_RETENTION_PRUNING_ENABLED`
//! today, against this single database, means **permanent, real data loss**
//! for anything outside the configured window: nothing else on the network
//! retains a second copy. The mechanism here (config, pruning logic, safety
//! checks around what never gets touched) is built correctly and is meant
//! to be genuinely safe *once* an archive-tier mirror actually exists; the
//! network-wide availability guarantee it should be gated on does not yet
//! exist structurally, so this stays an explicit, off-by-default opt-in an
//! operator must deliberately flip, with this warning, not a
//! quietly-defaulted convenience.
//!
//! Only `payload` is ever touched by pruning — never `entry_hash`,
//! `prev_hash`, `seq`, `kind`, `issuer`, `subject`, `event_timestamp`,
//! `version`, or `batch_id`. The Merkle tree (`crate::merkle`) is built
//! entirely from `entry_hash` values, never `payload` (issue #210), so a
//! pruned row stays fully present in, and verifiable against, the
//! commitment structure — only its content becomes locally unavailable
//! from *this* node. See [`crate::postgres::PostgresSettlementProvider::prune_payloads_older_than`]
//! for the actual `UPDATE`, and its `verify`/`list_entries` for how a
//! pruned row's now-missing content is handled without producing a false
//! "broken chain" report.

use time::{Duration, OffsetDateTime};

/// Which retention tier this node is configured to run as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTier {
    /// Complete history, no window. The default.
    Full,
    /// Only a configurable recent window of payloads is guaranteed to stay
    /// locally available — see the module-level doc comment for the
    /// availability caveat this carries at milestone-1 scale.
    Hot { window_days: i64 },
}

impl RetentionTier {
    /// The payload-retention cutoff this tier implies, evaluated at `now` —
    /// `None` for [`RetentionTier::Full`] (nothing is ever prunable), else
    /// `Some(now - window_days)`: any entry committed strictly before this
    /// instant is outside the configured window.
    pub fn cutoff(&self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        match self {
            RetentionTier::Full => None,
            RetentionTier::Hot { window_days } => Some(now - Duration::days(*window_days)),
        }
    }
}

/// A node's full retention configuration — its declared tier, plus the
/// separate, explicit opt-in that actually allows pruning to run at all.
/// Two independent gates on purpose: declaring `hot` alone commits a node
/// to *eventually* pruning down to its window, without yet accepting the
/// "nothing else retains a copy" risk described in the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionConfig {
    pub tier: RetentionTier,
    /// Off by default. Must be explicitly set `true` (`AVALON_RETENTION_PRUNING_ENABLED=true`)
    /// for [`Self::should_prune`] to ever return `true` — see the module
    /// doc comment for what turning this on means today.
    pub pruning_enabled: bool,
}

impl RetentionConfig {
    /// A full-tier node with pruning left off — the safe default returned
    /// whenever the relevant env vars are unset.
    pub const fn full() -> Self {
        Self {
            tier: RetentionTier::Full,
            pruning_enabled: false,
        }
    }

    /// Whether this node should actually run pruning: both "declared hot
    /// tier" and "pruning explicitly enabled" must hold. A full-tier node
    /// never prunes, even if `AVALON_RETENTION_PRUNING_ENABLED=true` was
    /// left set from a previous configuration — that flag is meaningless
    /// without a hot-tier window to prune down to, and is never
    /// misinterpreted as "prune everything."
    pub fn should_prune(&self) -> bool {
        matches!(self.tier, RetentionTier::Hot { .. }) && self.pruning_enabled
    }

    /// The cutoff pruning should use right now, or `None` if this node
    /// should not prune at all (see [`Self::should_prune`]).
    pub fn prune_cutoff(&self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        if !self.should_prune() {
            return None;
        }
        self.tier.cutoff(now)
    }

    /// Loads retention configuration from the environment, following the
    /// same `AVALON_NETWORK_ID`/`AVALON_SETTLEMENT_SIGNING_KEY` pattern
    /// this codebase already uses for node-declared configuration:
    ///
    /// - `AVALON_RETENTION_TIER` — `full` (default if unset) or `hot`.
    /// - `AVALON_RETENTION_HOT_WINDOW_DAYS` — required, positive integer,
    ///   only read when the tier is `hot`.
    /// - `AVALON_RETENTION_PRUNING_ENABLED` — `true`/`false` (default
    ///   `false`). Only meaningful for the `hot` tier; see
    ///   [`Self::should_prune`].
    pub fn from_env() -> Result<Self, RetentionConfigError> {
        Self::from_vars(
            std::env::var("AVALON_RETENTION_TIER").ok(),
            std::env::var("AVALON_RETENTION_HOT_WINDOW_DAYS").ok(),
            std::env::var("AVALON_RETENTION_PRUNING_ENABLED").ok(),
        )
    }

    /// The pure, testable core of [`Self::from_env`] — takes the three raw
    /// env-var values directly rather than reading the environment, so
    /// every combination is unit-testable without `std::env` mutation
    /// (which isn't thread-safe across parallel tests).
    fn from_vars(
        tier_var: Option<String>,
        window_var: Option<String>,
        pruning_var: Option<String>,
    ) -> Result<Self, RetentionConfigError> {
        let tier = match tier_var.as_deref().map(str::trim) {
            None | Some("") | Some("full") => RetentionTier::Full,
            Some("hot") => {
                let window_raw = window_var.ok_or(RetentionConfigError::MissingHotWindow)?;
                let window_days: i64 = window_raw
                    .trim()
                    .parse()
                    .map_err(|_| RetentionConfigError::InvalidHotWindow(window_raw.clone()))?;
                if window_days <= 0 {
                    return Err(RetentionConfigError::InvalidHotWindow(window_raw));
                }
                RetentionTier::Hot { window_days }
            }
            Some(other) => return Err(RetentionConfigError::UnknownTier(other.to_string())),
        };

        let pruning_enabled = match pruning_var.as_deref().map(str::trim) {
            None | Some("") => false,
            Some("true") | Some("1") => true,
            Some("false") | Some("0") => false,
            Some(other) => return Err(RetentionConfigError::InvalidPruningFlag(other.to_string())),
        };

        Ok(Self {
            tier,
            pruning_enabled,
        })
    }

    /// A short, operator-facing description — what `avalon-server` prints
    /// at startup alongside its `network_id` line, and what `avalon
    /// prune-ledger` prints before acting.
    pub fn describe(&self) -> String {
        match self.tier {
            RetentionTier::Full => "full/archive (no pruning; complete history retained)".into(),
            RetentionTier::Hot { window_days } => format!(
                "hot ({window_days}-day window; pruning {})",
                if self.pruning_enabled {
                    "ENABLED — payloads older than the window will be permanently discarded locally"
                } else {
                    "disabled — behaves as a full node until AVALON_RETENTION_PRUNING_ENABLED=true"
                }
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RetentionConfigError {
    #[error(
        "AVALON_RETENTION_TIER=hot requires AVALON_RETENTION_HOT_WINDOW_DAYS to be set — see .env.example"
    )]
    MissingHotWindow,
    #[error("AVALON_RETENTION_HOT_WINDOW_DAYS must be a positive integer, got {0:?}")]
    InvalidHotWindow(String),
    #[error("AVALON_RETENTION_TIER must be `full` or `hot`, got {0:?}")]
    UnknownTier(String),
    #[error("AVALON_RETENTION_PRUNING_ENABLED must be `true` or `false`, got {0:?}")]
    InvalidPruningFlag(String),
}

/// What one pruning pass did — returned by
/// [`crate::postgres::PostgresSettlementProvider::prune_payloads_older_than`]
/// and printed by `avalon prune-ledger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    pub cutoff: OffsetDateTime,
    pub pruned_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_full_tier_with_pruning_off_when_unset() {
        let config = RetentionConfig::from_vars(None, None, None).unwrap();
        assert_eq!(config.tier, RetentionTier::Full);
        assert!(!config.pruning_enabled);
        assert!(!config.should_prune());
    }

    #[test]
    fn explicit_full_tier_parses_the_same_as_unset() {
        let config = RetentionConfig::from_vars(Some("full".into()), None, None).unwrap();
        assert_eq!(config.tier, RetentionTier::Full);
    }

    #[test]
    fn hot_tier_requires_a_window() {
        let err = RetentionConfig::from_vars(Some("hot".into()), None, None).unwrap_err();
        assert!(matches!(err, RetentionConfigError::MissingHotWindow));
    }

    #[test]
    fn hot_tier_rejects_a_non_positive_window() {
        for bad in ["0", "-5", "not-a-number"] {
            let err = RetentionConfig::from_vars(Some("hot".into()), Some(bad.to_string()), None)
                .unwrap_err();
            assert!(matches!(err, RetentionConfigError::InvalidHotWindow(_)));
        }
    }

    #[test]
    fn hot_tier_with_valid_window_parses() {
        let config =
            RetentionConfig::from_vars(Some("hot".into()), Some("90".into()), None).unwrap();
        assert_eq!(config.tier, RetentionTier::Hot { window_days: 90 });
        assert!(!config.pruning_enabled, "pruning stays off by default");
        assert!(!config.should_prune());
    }

    #[test]
    fn unknown_tier_is_rejected() {
        let err = RetentionConfig::from_vars(Some("archive-of-everything".into()), None, None)
            .unwrap_err();
        assert!(matches!(err, RetentionConfigError::UnknownTier(_)));
    }

    #[test]
    fn pruning_flag_parses_true_and_false_variants() {
        for (raw, expected) in [("true", true), ("1", true), ("false", false), ("0", false)] {
            let config = RetentionConfig::from_vars(
                Some("hot".into()),
                Some("30".into()),
                Some(raw.to_string()),
            )
            .unwrap();
            assert_eq!(config.pruning_enabled, expected);
        }
    }

    #[test]
    fn invalid_pruning_flag_is_rejected() {
        let err = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("yes-please".into()),
        )
        .unwrap_err();
        assert!(matches!(err, RetentionConfigError::InvalidPruningFlag(_)));
    }

    /// The central safety gate: `pruning_enabled=true` on a *full*-tier
    /// node must never cause pruning, even though the flag technically
    /// parses fine on its own — `should_prune` requires the tier to be
    /// `Hot` too. This is what stops a stale `AVALON_RETENTION_PRUNING_ENABLED=true`
    /// left over from a previous hot-tier configuration from silently
    /// starting to prune the moment someone (mis)configures the tier back
    /// to `full`... it can't, because `should_prune` checks both.
    #[test]
    fn full_tier_never_prunes_even_if_pruning_flag_is_true() {
        let config = RetentionConfig {
            tier: RetentionTier::Full,
            pruning_enabled: true,
        };
        assert!(!config.should_prune());
        assert!(config.prune_cutoff(OffsetDateTime::now_utc()).is_none());
    }

    #[test]
    fn hot_tier_only_prunes_once_explicitly_enabled() {
        let disabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 30 },
            pruning_enabled: false,
        };
        assert!(!disabled.should_prune());
        assert!(disabled.prune_cutoff(OffsetDateTime::now_utc()).is_none());

        let enabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 30 },
            pruning_enabled: true,
        };
        assert!(enabled.should_prune());
        assert!(enabled.prune_cutoff(OffsetDateTime::now_utc()).is_some());
    }

    #[test]
    fn cutoff_is_exactly_window_days_before_now() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let tier = RetentionTier::Hot { window_days: 30 };
        let cutoff = tier.cutoff(now).unwrap();
        assert_eq!(cutoff, now - Duration::days(30));
    }

    #[test]
    fn full_tier_cutoff_is_always_none() {
        let now = OffsetDateTime::now_utc();
        assert_eq!(RetentionTier::Full.cutoff(now), None);
    }

    #[test]
    fn describe_full_tier_mentions_no_pruning() {
        let description = RetentionConfig::full().describe();
        assert!(description.contains("full"));
        assert!(description.to_lowercase().contains("no pruning"));
    }

    #[test]
    fn describe_hot_tier_states_whether_pruning_is_actually_enabled() {
        let disabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 90 },
            pruning_enabled: false,
        };
        assert!(disabled.describe().contains("disabled"));

        let enabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 90 },
            pruning_enabled: true,
        };
        assert!(enabled.describe().contains("ENABLED"));
    }
}
