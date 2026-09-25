//! Latency percentiles and rate-limit accounting.

use std::collections::BTreeMap;
use std::time::Duration;

/// Nearest-rank percentile of an ascending-sorted slice; `p` in 0..=100.
pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatencySummary {
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

pub fn summarize(samples: &[Duration]) -> LatencySummary {
    let mut sorted = samples.to_vec();
    sorted.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    LatencySummary {
        count: sorted.len(),
        p50_ms: ms(percentile(&sorted, 50.0)),
        p95_ms: ms(percentile(&sorted, 95.0)),
        p99_ms: ms(percentile(&sorted, 99.0)),
        max_ms: ms(sorted.last().copied().unwrap_or_default()),
    }
}

/// Most requests a GCRA limiter with `burst` capacity and a sustained rate of
/// `per_minute` can admit over `elapsed`.
pub fn max_admitted(per_minute: u64, burst: u64, elapsed: Duration) -> u64 {
    burst + (per_minute as f64 * elapsed.as_secs_f64() / 60.0).floor() as u64
}

/// Response status tally; status 0 counts transport errors.
#[derive(Debug, Default, Clone)]
pub struct StatusTally {
    pub by_status: BTreeMap<u16, u64>,
}

impl StatusTally {
    pub fn record(&mut self, status: u16) {
        *self.by_status.entry(status).or_default() += 1;
    }
    pub fn count(&self, status: u16) -> u64 {
        self.by_status.get(&status).copied().unwrap_or(0)
    }
    pub fn total(&self) -> u64 {
        self.by_status.values().sum()
    }
    pub fn ok(&self) -> u64 {
        self.by_status
            .iter()
            .filter(|(s, _)| (200..300).contains(*s))
            .map(|(_, n)| n)
            .sum()
    }
    /// 5xx responses and transport errors.
    pub fn server_errors(&self) -> u64 {
        self.by_status
            .iter()
            .filter(|(s, _)| **s >= 500 || **s == 0)
            .map(|(_, n)| n)
            .sum()
    }
    pub fn render(&self) -> String {
        self.by_status
            .iter()
            .map(|(s, n)| format!("{s}:{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let s: Vec<_> = (1..=100).map(ms).collect();
        assert_eq!(percentile(&s, 50.0), ms(50));
        assert_eq!(percentile(&s, 95.0), ms(95));
        assert_eq!(percentile(&s, 99.0), ms(99));
        assert_eq!(percentile(&s, 100.0), ms(100));
        assert_eq!(percentile(&s, 0.0), ms(1));
    }

    #[test]
    fn percentile_of_empty_and_single() {
        assert_eq!(percentile(&[], 99.0), Duration::ZERO);
        assert_eq!(percentile(&[ms(7)], 99.0), ms(7));
    }

    #[test]
    fn summarize_sorts_its_input() {
        let s = summarize(&[ms(30), ms(10), ms(20), ms(40)]);
        assert_eq!(s.count, 4);
        assert_eq!(s.p50_ms, 20.0);
        assert_eq!(s.max_ms, 40.0);
    }

    #[test]
    fn max_admitted_is_burst_plus_refill() {
        assert_eq!(max_admitted(600, 600, Duration::ZERO), 600);
        assert_eq!(max_admitted(600, 600, Duration::from_secs(6)), 660);
        assert_eq!(max_admitted(60, 60, Duration::from_millis(500)), 60);
    }

    #[test]
    fn tally_counts_by_class() {
        let mut t = StatusTally::default();
        for s in [200, 200, 204, 429, 503, 0] {
            t.record(s);
        }
        assert_eq!(t.total(), 6);
        assert_eq!(t.ok(), 3);
        assert_eq!(t.count(429), 1);
        assert_eq!(t.server_errors(), 2);
    }
}
