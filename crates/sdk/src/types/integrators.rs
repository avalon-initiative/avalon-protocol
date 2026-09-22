//! What kind of thing an integrator is, and the claim vocabulary that
//! follows from it.

use serde::{Deserialize, Serialize};

/// Integrators aren't only games (#324): an app or a service registers the
/// same way and issues the same kind of signed claim, just under its own
/// vocabulary. Fixed, closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegratorCategory {
    /// A game. Issues `achievement` claims.
    #[default]
    Game,
    /// A non-game application. Issues `milestone` claims.
    App,
    /// A service (a website, a bot, a tool). Issues `milestone` claims.
    Service,
}

impl IntegratorCategory {
    /// This category's permanent wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            IntegratorCategory::Game => "game",
            IntegratorCategory::App => "app",
            IntegratorCategory::Service => "service",
        }
    }

    /// Parses a wire string back into a category; `None` if unrecognized.
    pub fn parse(s: &str) -> Option<IntegratorCategory> {
        Some(match s {
            "game" => IntegratorCategory::Game,
            "app" => IntegratorCategory::App,
            "service" => IntegratorCategory::Service,
            _ => return None,
        })
    }

    /// The claim-vocabulary word this category's issuers use: `Game`
    /// issuers say `"achievement"`, `App`/`Service` issuers share
    /// `"milestone"`. Used both as the `GlobalId` "kind" segment
    /// (`game:<slug>:achievement:<key>` vs. `app:<slug>:milestone:<key>`)
    /// and as the event-kind prefix — and, load-bearing here, as the
    /// `claim_kind` folded into attestation signing bytes
    /// (`crate::achievements`), so a signature produced under one
    /// vocabulary can never be replayed as the other.
    pub fn claim_kind(&self) -> &'static str {
        match self {
            IntegratorCategory::Game => "achievement",
            IntegratorCategory::App | IntegratorCategory::Service => "milestone",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_round_trips_through_its_wire_string() {
        for category in [
            IntegratorCategory::Game,
            IntegratorCategory::App,
            IntegratorCategory::Service,
        ] {
            assert_eq!(IntegratorCategory::parse(category.as_str()), Some(category));
        }
        assert_eq!(IntegratorCategory::parse("website"), None);
    }

    #[test]
    fn claim_kind_splits_games_from_everything_else() {
        assert_eq!(IntegratorCategory::Game.claim_kind(), "achievement");
        assert_eq!(IntegratorCategory::App.claim_kind(), "milestone");
        assert_eq!(IntegratorCategory::Service.claim_kind(), "milestone");
    }
}
