//! Keeps a repeating warning to one line per key per interval, with a count of what was held back.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Most keys tracked; the table restarts when it is reached so a flood of keys cannot grow it.
const MAX_KEYS: usize = 1024;

pub struct LogThrottle {
    interval: Duration,
    seen: Mutex<HashMap<String, (Instant, u64)>>,
}

impl LogThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// `Some(suppressed)` when `key` may log now, `suppressed` being the occurrences held back
    /// since its last line; `None` when it is inside its interval.
    pub fn permit(&self, key: &str, now: Instant) -> Option<u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.len() >= MAX_KEYS && !seen.contains_key(key) {
            seen.clear();
        }
        match seen.get_mut(key) {
            Some((last, held)) if now.duration_since(*last) < self.interval => {
                *held += 1;
                None
            }
            Some(entry) => Some(std::mem::replace(entry, (now, 0)).1),
            None => {
                seen.insert(key.to_string(), (now, 0));
                Some(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_per_key_per_interval_with_the_held_back_count() {
        let t = LogThrottle::new(Duration::from_secs(10));
        let t0 = Instant::now();
        assert_eq!(t.permit("a", t0), Some(0));
        assert_eq!(t.permit("a", t0 + Duration::from_secs(1)), None);
        assert_eq!(t.permit("a", t0 + Duration::from_secs(2)), None);
        assert_eq!(t.permit("b", t0 + Duration::from_secs(2)), Some(0));
        assert_eq!(t.permit("a", t0 + Duration::from_secs(10)), Some(2));
        assert_eq!(t.permit("a", t0 + Duration::from_secs(11)), None);
    }

    #[test]
    fn the_key_table_stays_bounded() {
        let t = LogThrottle::new(Duration::from_secs(10));
        let now = Instant::now();
        for i in 0..5000 {
            t.permit(&i.to_string(), now);
        }
        assert!(t.seen.lock().unwrap().len() <= MAX_KEYS);
    }
}
