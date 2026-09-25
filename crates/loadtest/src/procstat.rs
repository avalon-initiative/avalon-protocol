//! CPU and memory sampling of one server process from procfs (Linux).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Kernel clock ticks per second; 100 on every mainstream Linux build.
const CLK_TCK: f64 = 100.0;

/// utime + stime in clock ticks from the contents of `/proc/<pid>/stat`.
pub fn parse_stat_ticks(stat: &str) -> Option<u64> {
    // The command name may contain spaces and parentheses; fields resume after the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Resident set size in KiB from the contents of `/proc/<pid>/status`.
pub fn parse_vmrss_kb(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[derive(Debug, Clone, Default)]
pub struct ProcUsage {
    pub peak_rss_mb: f64,
    pub avg_rss_mb: f64,
    /// Average CPU over the window, in percent of one core.
    pub avg_cpu_pct: f64,
    pub peak_cpu_pct: f64,
    pub samples: usize,
}

pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<ProcUsage>,
}

fn read_sample(pid: u32) -> Option<(u64, u64)> {
    let ticks = parse_stat_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    let rss = parse_vmrss_kb(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)?;
    Some((ticks, rss))
}

impl Sampler {
    pub fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = tokio::spawn(async move {
            let mut usage = ProcUsage::default();
            let first = read_sample(pid);
            let started = Instant::now();
            let mut last = first.map(|(t, _)| (t, started));
            let mut rss_sum = 0.0;
            while !flag.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let Some((ticks, rss_kb)) = read_sample(pid) else {
                    break;
                };
                let now = Instant::now();
                let rss_mb = rss_kb as f64 / 1024.0;
                usage.peak_rss_mb = usage.peak_rss_mb.max(rss_mb);
                rss_sum += rss_mb;
                usage.samples += 1;
                if let Some((prev_ticks, prev_at)) = last {
                    let dt = now.duration_since(prev_at).as_secs_f64();
                    if dt > 0.0 {
                        let pct = (ticks - prev_ticks) as f64 / CLK_TCK / dt * 100.0;
                        usage.peak_cpu_pct = usage.peak_cpu_pct.max(pct);
                    }
                }
                last = Some((ticks, now));
            }
            if let (Some((t0, _)), Some((t1, _))) = (first, read_sample(pid)) {
                let dt = started.elapsed().as_secs_f64();
                if dt > 0.0 {
                    usage.avg_cpu_pct = (t1 - t0) as f64 / CLK_TCK / dt * 100.0;
                }
            }
            if usage.samples > 0 {
                usage.avg_rss_mb = rss_sum / usage.samples as f64;
            }
            usage
        });
        Self { stop, handle }
    }

    pub async fn finish(self) -> ProcUsage {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.await.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_ticks_handle_awkward_command_names() {
        let stat = "1234 (avalon (server) x) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 8 0 100 1000000 5000 18446744073709551615";
        assert_eq!(parse_stat_ticks(stat), Some(300));
    }

    #[test]
    fn stat_ticks_reject_truncated_input() {
        assert_eq!(parse_stat_ticks("1234 (x) S 1"), None);
        assert_eq!(parse_stat_ticks("garbage"), None);
    }

    #[test]
    fn vmrss_is_parsed_in_kib() {
        let status = "Name:\tx\nVmPeak:\t 9000 kB\nVmRSS:\t  4321 kB\nThreads:\t4\n";
        assert_eq!(parse_vmrss_kb(status), Some(4321));
        assert_eq!(parse_vmrss_kb("Name:\tx\n"), None);
    }
}
