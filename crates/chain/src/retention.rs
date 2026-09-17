//! Node-tiered durable history retention — issue #208, implementing #180's
//! decision. See `docs/architecture/nodes.md`'s "Settlement retention
//! tiers" section for the two-tier design, the env config, and the
//! honest milestone-1 caveat that pruning today means real permanent
//! data loss since no archive-tier mirror network exists yet.
//!
//! **Archive-confirmation gating (issue #569, closing the gap the note
//! above used to describe as pure operator discipline).** `archive_peers`/
//! `min_archive_confirmations` below are this crate's pure config half —
//! the actual HTTP confirmation check (querying each peer's own mirrored
//! progress before pruning) lives in `avalon_server::retention`, since
//! this crate deliberately has no network I/O of its own. See that
//! module's doc comment for the full mechanism.

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionConfig {
    pub tier: RetentionTier,
    /// Off by default. Must be explicitly set `true` (`AVALON_RETENTION_PRUNING_ENABLED=true`)
    /// for [`Self::should_prune`] to ever return `true` — see the module
    /// doc comment for what turning this on means today.
    pub pruning_enabled: bool,
    /// Issue #569: base URLs of nodes expected to have mirrored this
    /// node's own network before pruning is allowed to proceed past what
    /// they've confirmed. Empty (the default) means no gating at all —
    /// additive, opt-in, same posture every other retention knob here
    /// takes; a node that never sets this behaves exactly as before this
    /// existed.
    pub archive_peers: Vec<String>,
    /// How many *distinct* `archive_peers` must confirm coverage up to a
    /// pruning pass's boundary before it's allowed to run. Meaningless
    /// (never checked) when `archive_peers` is empty.
    pub min_archive_confirmations: usize,
}

impl RetentionConfig {
    /// A full-tier node with pruning left off — the safe default returned
    /// whenever the relevant env vars are unset.
    pub fn full() -> Self {
        Self {
            tier: RetentionTier::Full,
            pruning_enabled: false,
            archive_peers: Vec::new(),
            min_archive_confirmations: 0,
        }
    }

    /// Issue #569: whether a pruning pass must first confirm archive
    /// coverage before it's allowed to run — `false` (no gating, today's
    /// original milestone-1 behavior) whenever `archive_peers` is empty,
    /// regardless of `min_archive_confirmations`.
    pub fn requires_archive_confirmation(&self) -> bool {
        !self.archive_peers.is_empty() && self.min_archive_confirmations > 0
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
    /// - `AVALON_RETENTION_ARCHIVE_PEERS` (issue #569) — comma-separated
    ///   base URLs of nodes expected to mirror this one; unset/empty means
    ///   no archive-confirmation gating at all.
    /// - `AVALON_RETENTION_MIN_ARCHIVE_CONFIRMATIONS` — positive integer,
    ///   only read when `AVALON_RETENTION_ARCHIVE_PEERS` is non-empty;
    ///   defaults to `1` when peers are configured but this is left unset.
    pub fn from_env() -> Result<Self, RetentionConfigError> {
        Self::from_vars(
            std::env::var("AVALON_RETENTION_TIER").ok(),
            std::env::var("AVALON_RETENTION_HOT_WINDOW_DAYS").ok(),
            std::env::var("AVALON_RETENTION_PRUNING_ENABLED").ok(),
            std::env::var("AVALON_RETENTION_ARCHIVE_PEERS").ok(),
            std::env::var("AVALON_RETENTION_MIN_ARCHIVE_CONFIRMATIONS").ok(),
        )
    }

    /// The pure, testable core of [`Self::from_env`] — takes the raw
    /// env-var values directly rather than reading the environment, so
    /// every combination is unit-testable without `std::env` mutation
    /// (which isn't thread-safe across parallel tests).
    fn from_vars(
        tier_var: Option<String>,
        window_var: Option<String>,
        pruning_var: Option<String>,
        archive_peers_var: Option<String>,
        min_archive_confirmations_var: Option<String>,
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

        let archive_peers: Vec<String> = archive_peers_var
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let min_archive_confirmations = if archive_peers.is_empty() {
            0
        } else {
            match min_archive_confirmations_var.as_deref().map(str::trim) {
                None | Some("") => 1,
                Some(raw) => raw.parse::<usize>().map_err(|_| {
                    RetentionConfigError::InvalidMinArchiveConfirmations(raw.to_string())
                })?,
            }
        };

        Ok(Self {
            tier,
            pruning_enabled,
            archive_peers,
            min_archive_confirmations,
        })
    }

    /// A short, operator-facing description — what `avalon-server` prints
    /// at startup alongside its `network_id` line, and what `avalon
    /// prune-ledger` prints before acting.
    pub fn describe(&self) -> String {
        let base = match self.tier {
            RetentionTier::Full => "full/archive (no pruning; complete history retained)".into(),
            RetentionTier::Hot { window_days } => format!(
                "hot ({window_days}-day window; pruning {})",
                if self.pruning_enabled {
                    "ENABLED — payloads older than the window will be permanently discarded locally"
                } else {
                    "disabled — behaves as a full node until AVALON_RETENTION_PRUNING_ENABLED=true"
                }
            ),
        };
        if self.requires_archive_confirmation() {
            format!(
                "{base}; gated on {} archive confirmation(s) among {} configured peer(s) before each prune",
                self.min_archive_confirmations,
                self.archive_peers.len()
            )
        } else {
            base
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
    #[error(
        "AVALON_RETENTION_MIN_ARCHIVE_CONFIRMATIONS must be a non-negative integer, got {0:?}"
    )]
    InvalidMinArchiveConfirmations(String),
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
        let config = RetentionConfig::from_vars(None, None, None, None, None).unwrap();
        assert_eq!(config.tier, RetentionTier::Full);
        assert!(!config.pruning_enabled);
        assert!(!config.should_prune());
    }

    #[test]
    fn explicit_full_tier_parses_the_same_as_unset() {
        let config =
            RetentionConfig::from_vars(Some("full".into()), None, None, None, None).unwrap();
        assert_eq!(config.tier, RetentionTier::Full);
    }

    #[test]
    fn hot_tier_requires_a_window() {
        let err =
            RetentionConfig::from_vars(Some("hot".into()), None, None, None, None).unwrap_err();
        assert!(matches!(err, RetentionConfigError::MissingHotWindow));
    }

    #[test]
    fn hot_tier_rejects_a_non_positive_window() {
        for bad in ["0", "-5", "not-a-number"] {
            let err = RetentionConfig::from_vars(
                Some("hot".into()),
                Some(bad.to_string()),
                None,
                None,
                None,
            )
            .unwrap_err();
            assert!(matches!(err, RetentionConfigError::InvalidHotWindow(_)));
        }
    }

    #[test]
    fn hot_tier_with_valid_window_parses() {
        let config =
            RetentionConfig::from_vars(Some("hot".into()), Some("90".into()), None, None, None)
                .unwrap();
        assert_eq!(config.tier, RetentionTier::Hot { window_days: 90 });
        assert!(!config.pruning_enabled, "pruning stays off by default");
        assert!(!config.should_prune());
    }

    #[test]
    fn unknown_tier_is_rejected() {
        let err = RetentionConfig::from_vars(
            Some("archive-of-everything".into()),
            None,
            None,
            None,
            None,
        )
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
                None,
                None,
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
            None,
            None,
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
            ..RetentionConfig::full()
        };
        assert!(!config.should_prune());
        assert!(config.prune_cutoff(OffsetDateTime::now_utc()).is_none());
    }

    #[test]
    fn hot_tier_only_prunes_once_explicitly_enabled() {
        let disabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 30 },
            pruning_enabled: false,
            ..RetentionConfig::full()
        };
        assert!(!disabled.should_prune());
        assert!(disabled.prune_cutoff(OffsetDateTime::now_utc()).is_none());

        let enabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 30 },
            pruning_enabled: true,
            ..RetentionConfig::full()
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
            ..RetentionConfig::full()
        };
        assert!(disabled.describe().contains("disabled"));

        let enabled = RetentionConfig {
            tier: RetentionTier::Hot { window_days: 90 },
            pruning_enabled: true,
            ..RetentionConfig::full()
        };
        assert!(enabled.describe().contains("ENABLED"));
    }

    #[test]
    fn no_archive_peers_means_no_confirmation_required() {
        let config = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("true".into()),
            None,
            None,
        )
        .unwrap();
        assert!(config.archive_peers.is_empty());
        assert_eq!(config.min_archive_confirmations, 0);
        assert!(!config.requires_archive_confirmation());
    }

    #[test]
    fn archive_peers_default_to_requiring_one_confirmation() {
        let config = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("true".into()),
            Some("http://a.example,http://b.example/".into()),
            None,
        )
        .unwrap();
        assert_eq!(
            config.archive_peers,
            vec![
                "http://a.example".to_string(),
                "http://b.example".to_string()
            ],
            "trailing slashes are stripped, matching every other peer-URL parser in this codebase"
        );
        assert_eq!(config.min_archive_confirmations, 1);
        assert!(config.requires_archive_confirmation());
    }

    #[test]
    fn archive_peers_honors_an_explicit_confirmation_count() {
        let config = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("true".into()),
            Some("http://a.example,http://b.example,http://c.example".into()),
            Some("2".into()),
        )
        .unwrap();
        assert_eq!(config.min_archive_confirmations, 2);
        assert!(config.requires_archive_confirmation());
    }

    #[test]
    fn an_explicit_zero_confirmation_count_disables_gating_despite_configured_peers() {
        let config = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("true".into()),
            Some("http://a.example".into()),
            Some("0".into()),
        )
        .unwrap();
        assert!(
            !config.requires_archive_confirmation(),
            "an operator explicitly setting 0 is a deliberate opt-out, not a bug"
        );
    }

    #[test]
    fn invalid_min_archive_confirmations_is_rejected() {
        let err = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("true".into()),
            Some("http://a.example".into()),
            Some("not-a-number".into()),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RetentionConfigError::InvalidMinArchiveConfirmations(_)
        ));
    }

    #[test]
    fn describe_mentions_archive_gating_when_configured() {
        let config = RetentionConfig::from_vars(
            Some("hot".into()),
            Some("30".into()),
            Some("true".into()),
            Some("http://a.example,http://b.example".into()),
            Some("2".into()),
        )
        .unwrap();
        let description = config.describe();
        assert!(description.contains("2 archive confirmation"));
        assert!(description.contains("2 configured peer"));
    }
}
