//! Protocol constants and pure checks for guardian recovery. The delay and the minimum threshold are
//! fixed by the protocol, never by a node's settings: every mirror must reach the same verdict.

use time::{Duration, OffsetDateTime};

/// How long the request and the configuration it relies on must have been covered by cosigned heads,
/// measured between heads and never on a node's clock.
pub const RECOVERY_DELAY: Duration = Duration::hours(48);

/// The fewest guardian approvals any recovery needs, whatever threshold the owner configured.
pub const MIN_RECOVERY_THRESHOLD: i32 = 2;

/// Approvals a recovery needs under a configured threshold.
pub fn required_approvals(configured_threshold: i32) -> usize {
    configured_threshold.max(MIN_RECOVERY_THRESHOLD) as usize
}

/// Whether `reference` (the time of the latest cosigned head) is at least [`RECOVERY_DELAY`] after
/// `anchor` (the witness-signed quorum time of the first cosigned head covering the entry).
pub fn delay_elapsed(anchor: OffsetDateTime, reference: OffsetDateTime) -> bool {
    reference - anchor >= RECOVERY_DELAY
}

/// A 32-byte hash from its 64 lowercase hex characters.
pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex::decode(text).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_threshold_never_falls_below_the_minimum() {
        assert_eq!(required_approvals(1), 2);
        assert_eq!(required_approvals(2), 2);
        assert_eq!(required_approvals(5), 5);
    }

    #[test]
    fn the_delay_is_measured_between_heads() {
        let t = OffsetDateTime::UNIX_EPOCH;
        assert!(!delay_elapsed(t, t + RECOVERY_DELAY - Duration::seconds(1)));
        assert!(delay_elapsed(t, t + RECOVERY_DELAY));
        assert!(!delay_elapsed(t + RECOVERY_DELAY, t));
    }

    #[test]
    fn hashes_parse_strictly() {
        assert!(parse_hex32(&"ab".repeat(32)).is_some());
        for bad in [
            "",
            "ab",
            &"AB".repeat(32),
            &"zz".repeat(32),
            &"ab".repeat(33),
        ] {
            assert!(parse_hex32(bad).is_none());
        }
    }
}
