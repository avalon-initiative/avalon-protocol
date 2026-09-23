//! A revocation/deletion's coded reason — replaces the
//! free-text `reason_code: String` `attestation_revocations` and
//! `game_data.deleted` both used, with a real, extensible vocabulary that
//! *means* something: whether the original claim/instance should keep
//! showing up (marked revoked/deleted) in a current-state projection, or
//! disappear from it entirely as if it never existed.
//!
//! Same `Known`/`Other` open-enum shape
//! [`crate::events::ProtocolEventKind`] already established for
//! `ProtocolEvent::kind`, for the identical reason: an old
//! build must never choke on a reason code introduced after it shipped —
//! [`RevocationReasonCode::from_str`] is infallible, and an unrecognized
//! code falls back to [`RevocationReasonCode::Other`] rather than an
//! error. Adding a new known reason later is purely additive; it never
//! changes what an existing revoked entry's *recorded* code means.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The fixed, known vocabulary. Not exhaustive by design (the ticket's own
/// text: "doesn't need to be fully enumerated") — [`RevocationReasonCode::Other`]
/// covers everything not listed here, including a future addition an
/// older build hasn't learned about yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationReasonCodeVariant {
    /// The claim/instance was real and correctly earned/created, and the
    /// *fact that it was later revoked* is itself trust-relevant history
    /// (e.g. an achievement pulled for cheating). Stays visible, marked
    /// revoked — the worked example `docs/architecture/revocation.md`
    /// already documents for scenario C.
    Cheating,
    /// The claim/instance should never have existed in the first place —
    /// an operator/integrator error, not a fact about the subject (e.g.
    /// something published to prod by mistake). Showing "subject had X,
    /// then had it revoked" is actively misleading here, so it's hidden
    /// from current-state views entirely once revoked.
    Mistake,
    /// A duplicate of another still-valid claim/instance — same
    /// "shouldn't have existed as its own entry" shape as `Mistake`,
    /// hidden once revoked.
    Duplicate,
    /// A policy or ruleset changed after the fact (e.g. an achievement
    /// definition retired network-wide) — not an error and not about the
    /// subject specifically, so the historical fact of having earned it
    /// under the old policy stays visible, same as `Cheating`.
    PolicyChange,
}

impl RevocationReasonCodeVariant {
    pub const KNOWN: &'static [Self] = &[
        Self::Cheating,
        Self::Mistake,
        Self::Duplicate,
        Self::PolicyChange,
    ];

    /// The permanent wire string. Never changes once shipped — same
    /// invariant `ProtocolEventKindVariant::as_str` documents for the
    /// identical reason (this string is durably recorded in ledger
    /// history and revocation rows, not just wire payloads).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cheating => "cheating",
            Self::Mistake => "mistake",
            Self::Duplicate => "duplicate",
            Self::PolicyChange => "policy_change",
        }
    }

    /// Whether a claim/instance revoked/deleted for this reason should
    /// disappear from current-state projection views entirely (`true`),
    /// or stay visible, marked revoked/deleted (`false`) — #534's actual
    /// behavioral point. Defaults are deliberately conservative: only
    /// reasons that mean "this shouldn't have existed" hide; anything
    /// that's a real fact about the subject stays visible.
    pub fn hides_after_revocation(&self) -> bool {
        matches!(self, Self::Mistake | Self::Duplicate)
    }
}

/// A `reason_code` — either one of [`RevocationReasonCodeVariant`]'s known
/// codes, or [`RevocationReasonCode::Other`] for anything not yet known to
/// this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationReasonCode {
    Known(RevocationReasonCodeVariant),
    Other(String),
}

impl RevocationReasonCode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(v) => v.as_str(),
            Self::Other(s) => s,
        }
    }

    /// See [`RevocationReasonCodeVariant::hides_after_revocation`]. An
    /// unrecognized code (this build doesn't know what it means)
    /// defaults to `false` — staying visible is the safe failure mode;
    /// silently hiding history because a code wasn't recognized would be
    /// the wrong default to fail toward.
    pub fn hides_after_revocation(&self) -> bool {
        match self {
            Self::Known(v) => v.hides_after_revocation(),
            Self::Other(_) => false,
        }
    }
}

impl From<RevocationReasonCodeVariant> for RevocationReasonCode {
    fn from(v: RevocationReasonCodeVariant) -> Self {
        RevocationReasonCode::Known(v)
    }
}

impl fmt::Display for RevocationReasonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Infallible on purpose — see this module's own doc comment.
impl FromStr for RevocationReasonCode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for known in RevocationReasonCodeVariant::KNOWN {
            if known.as_str() == s {
                return Ok(RevocationReasonCode::Known(*known));
            }
        }
        Ok(RevocationReasonCode::Other(s.to_string()))
    }
}

impl From<String> for RevocationReasonCode {
    fn from(s: String) -> Self {
        // `FromStr::from_str` is infallible for this type.
        s.parse().unwrap_or(RevocationReasonCode::Other(s))
    }
}

impl Serialize for RevocationReasonCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RevocationReasonCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(RevocationReasonCode::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheating_stays_visible() {
        assert!(
            !RevocationReasonCode::Known(RevocationReasonCodeVariant::Cheating)
                .hides_after_revocation()
        );
    }

    #[test]
    fn mistake_hides() {
        assert!(
            RevocationReasonCode::Known(RevocationReasonCodeVariant::Mistake)
                .hides_after_revocation()
        );
    }

    #[test]
    fn duplicate_hides() {
        assert!(
            RevocationReasonCode::Known(RevocationReasonCodeVariant::Duplicate)
                .hides_after_revocation()
        );
    }

    #[test]
    fn policy_change_stays_visible() {
        assert!(
            !RevocationReasonCode::Known(RevocationReasonCodeVariant::PolicyChange)
                .hides_after_revocation()
        );
    }

    #[test]
    fn unrecognized_code_defaults_to_visible() {
        assert!(
            !RevocationReasonCode::from("some_future_code".to_string()).hides_after_revocation()
        );
    }

    #[test]
    fn round_trips_through_json_as_a_plain_string() {
        let code = RevocationReasonCode::Known(RevocationReasonCodeVariant::Mistake);
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "\"mistake\"");
        let parsed: RevocationReasonCode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, code);
    }

    #[test]
    fn unrecognized_json_string_decodes_to_other_not_an_error() {
        let parsed: RevocationReasonCode = serde_json::from_str("\"brand_new_reason\"").unwrap();
        assert_eq!(
            parsed,
            RevocationReasonCode::Other("brand_new_reason".to_string())
        );
    }
}
