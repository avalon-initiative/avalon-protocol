//! Host resource metrics for `/nodes/status`'s `resources` block — issue
//! #517. Cross-platform via the `sysinfo` crate rather than hand-rolled
//! `/proc` parsing: hosters run on whatever OS they choose, not just
//! Linux, and `sysinfo` already abstracts CPU/memory/disk/process reads
//! across Linux/macOS/Windows/BSD so this doesn't need its own per-OS
//! branches.
//!
//! Everything here is best-effort and diagnostic only, per the ticket's
//! own invariants: a metric this process can't read on the host it happens
//! to be running on is `None`, never a request failure, and nothing in the
//! protocol (peer admission, mirroring, consensus) ever reads any of it —
//! same posture `nodes::NodeStatusResponse::stale` already has.
//!
//! CPU usage percentage is only meaningful as a delta between two
//! `sysinfo` refreshes a short interval apart — computing that inline on
//! every `/nodes/status` call would make each request pay for that delay.
//! Instead, [`start_sampler`] runs on its own timer and keeps a shared
//! snapshot ([`HostMetricsSampler`]) that the request handler just reads.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuMetrics {
    pub core_count: Option<usize>,
    pub usage_percent: Option<f32>,
    pub load_average_1m: Option<f64>,
    pub load_average_5m: Option<f64>,
    pub load_average_15m: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryMetrics {
    pub used_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
}

/// One configured path's disk usage — `label` says which config value it
/// came from (`"node_storage"` / `"postgres_data"`), since a node may
/// report more than one and a dashboard client needs to tell them apart.
#[derive(Debug, Clone, Serialize)]
pub struct DiskMetrics {
    pub label: String,
    pub mount_point: Option<String>,
    pub used_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DbPoolMetrics {
    pub size: Option<u32>,
    pub in_use: Option<u32>,
}

/// The `resources` block itself — every field individually optional per
/// #517's acceptance criteria, so a client can render whatever a given
/// node/platform actually managed to report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeResourceMetrics {
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub disks: Vec<DiskMetrics>,
    pub process_uptime_seconds: Option<u64>,
    /// This process's open file descriptor count — the cheapest
    /// cross-platform proxy `sysinfo` exposes for "how many
    /// connections/handles is this node currently holding open," since
    /// `sysinfo` has no direct portable socket-count API.
    pub open_file_count: Option<usize>,
    pub db_pool: DbPoolMetrics,
}

#[derive(Default, Clone)]
struct Snapshot {
    cpu: CpuMetrics,
    memory: MemoryMetrics,
    disks: Vec<DiskMetrics>,
    open_file_count: Option<usize>,
}

/// Shared, periodically-refreshed host metrics — cheap to clone (`Arc`
/// inside) into [`crate::state::AppState`]. Reading [`Self::current`]
/// never blocks on `sysinfo` itself; [`start_sampler`] is the only thing
/// that ever touches a live `System`/`Disks` handle.
#[derive(Clone)]
pub struct HostMetricsSampler {
    snapshot: Arc<RwLock<Snapshot>>,
    started_at: Instant,
    /// `(label, path)` pairs to report disk usage for, resolved once at
    /// startup by [`Self::from_env`].
    disk_paths: Vec<(String, PathBuf)>,
}

impl HostMetricsSampler {
    pub fn new(disk_paths: Vec<(String, PathBuf)>) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(Snapshot::default())),
            started_at: Instant::now(),
            disk_paths,
        }
    }

    /// `AVALON_NODE_STORAGE_PATH` (defaulting to this process's current
    /// working directory when unset) is always reported. `AVALON_POSTGRES_DATA_PATH`
    /// is reported too, but only makes sense when Postgres actually runs
    /// on this same host with a locally-readable data directory — a
    /// deployment pointing at a remote/managed Postgres (this sandbox's
    /// own `.env` does exactly that) simply leaves it unset, and disk
    /// reporting omits that entry rather than failing.
    pub fn from_env() -> Self {
        let mut disk_paths = Vec::new();

        let node_storage = std::env::var("AVALON_NODE_STORAGE_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok());
        if let Some(path) = node_storage {
            disk_paths.push(("node_storage".to_string(), path));
        }

        if let Some(path) = std::env::var("AVALON_POSTGRES_DATA_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
        {
            disk_paths.push(("postgres_data".to_string(), path));
        }

        Self::new(disk_paths)
    }

    /// Refreshes the shared snapshot in place. `sys`/`disks` are threaded
    /// in rather than constructed fresh each tick — `sysinfo` computes CPU
    /// usage % from the delta since its *previous* refresh, so a fresh
    /// `System` every tick would report 0% forever.
    fn refresh(&self, sys: &mut System, disks: &mut Disks) {
        sys.refresh_cpu_usage();
        sys.refresh_memory();

        let pid = Pid::from_u32(std::process::id());
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        disks.refresh(true);

        let load = System::load_average();
        let cpu = CpuMetrics {
            core_count: (!sys.cpus().is_empty()).then(|| sys.cpus().len()),
            usage_percent: Some(sys.global_cpu_usage()),
            load_average_1m: Some(load.one),
            load_average_5m: Some(load.five),
            load_average_15m: Some(load.fifteen),
        };
        let memory = MemoryMetrics {
            used_bytes: Some(sys.used_memory()),
            total_bytes: Some(sys.total_memory()),
            swap_used_bytes: Some(sys.used_swap()),
            swap_total_bytes: Some(sys.total_swap()),
        };
        let open_file_count = sys.process(pid).and_then(|p| p.open_files());

        let disk_metrics = self
            .disk_paths
            .iter()
            .map(|(label, path)| disk_metrics_for(label, path, disks))
            .collect();

        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = Snapshot {
                cpu,
                memory,
                disks: disk_metrics,
                open_file_count,
            };
        }
    }

    /// Best-effort read of the latest snapshot plus this process's own
    /// uptime (tracked independently of `sysinfo`, so it's always
    /// available even before the sampler's first tick). Never fails; an
    /// unpopulated snapshot (read before the first tick, or the sampler
    /// having never started) just means every `sysinfo`-derived field is
    /// `None`/empty.
    pub fn current(
        &self,
    ) -> (
        CpuMetrics,
        MemoryMetrics,
        Vec<DiskMetrics>,
        Option<usize>,
        u64,
    ) {
        let uptime = self.started_at.elapsed().as_secs();
        match self.snapshot.read() {
            Ok(snapshot) => (
                snapshot.cpu.clone(),
                snapshot.memory.clone(),
                snapshot.disks.clone(),
                snapshot.open_file_count,
                uptime,
            ),
            Err(_) => (
                CpuMetrics::default(),
                MemoryMetrics::default(),
                Vec::new(),
                None,
                uptime,
            ),
        }
    }
}

/// Matches `path` against `disks`' mount points, picking the deepest
/// (longest) one that actually contains it — the same "most specific
/// mount wins" logic `df`/`findmnt` use, so a storage path under `/data`
/// reports `/data`'s usage rather than `/`'s when both are mounted.
fn disk_metrics_for(label: &str, path: &std::path::Path, disks: &Disks) -> DiskMetrics {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let best = disks
        .list()
        .iter()
        .filter(|d| canonical.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len());

    match best {
        Some(disk) => DiskMetrics {
            label: label.to_string(),
            mount_point: Some(disk.mount_point().to_string_lossy().to_string()),
            used_bytes: Some(disk.total_space().saturating_sub(disk.available_space())),
            total_bytes: Some(disk.total_space()),
        },
        None => DiskMetrics {
            label: label.to_string(),
            mount_point: None,
            used_bytes: None,
            total_bytes: None,
        },
    }
}

/// Spawned once at startup (see `main.rs`), unconditionally — cheap enough
/// (a handful of reads every few seconds) that it doesn't need its own
/// opt-in env var, unlike heavier background workers this crate gates on
/// config. Never returns.
pub async fn start_sampler(sampler: HostMetricsSampler) {
    let mut sys = System::new_all();
    let mut disks = Disks::new_with_refreshed_list();

    // `sysinfo`'s CPU usage % needs an initial baseline reading before the
    // first delta against it means anything.
    sys.refresh_cpu_usage();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;

    const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
    loop {
        sampler.refresh(&mut sys, &mut disks);
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #517's own invariant: a metric this process can't read is `None`,
    /// never a request failure. Reading a sampler before its background
    /// task has ever ticked (or one with no configured disk paths at all)
    /// is the simplest case that can produce that — every `sysinfo`-derived
    /// field must come back `None`/empty rather than panicking, and the
    /// resulting `NodeResourceMetrics` must still serialize to valid JSON.
    #[test]
    fn an_unrefreshed_sampler_reports_no_metrics_but_still_serializes() {
        let sampler = HostMetricsSampler::new(Vec::new());
        let (cpu, memory, disks, open_file_count, uptime_seconds) = sampler.current();

        assert_eq!(cpu.core_count, None);
        assert_eq!(cpu.usage_percent, None);
        assert_eq!(memory.used_bytes, None);
        assert!(disks.is_empty());
        assert_eq!(open_file_count, None);

        let metrics = NodeResourceMetrics {
            cpu,
            memory,
            disks,
            process_uptime_seconds: Some(uptime_seconds),
            open_file_count,
            db_pool: DbPoolMetrics::default(),
        };
        let json = serde_json::to_value(&metrics).expect("must serialize even when empty");
        assert!(json.get("cpu").is_some());
        assert!(json.get("memory").is_some());
        assert_eq!(json["cpu"]["core_count"], serde_json::Value::Null);
    }

    /// A nonexistent path never panics — it either falls back to whatever
    /// real mount point still contains it (e.g. `/` on Unix, since every
    /// absolute path is "under" the root mount), or `None` when this
    /// sandbox's `Disks` list came back empty. Either outcome is fine; the
    /// only thing #517 requires is that this never errors the request.
    #[test]
    fn disk_metrics_for_a_nonexistent_path_never_panics() {
        let disks = Disks::new_with_refreshed_list();
        let metrics = disk_metrics_for(
            "node_storage",
            std::path::Path::new("/this/path/almost-certainly/does/not/exist-517"),
            &disks,
        );
        assert_eq!(metrics.label, "node_storage");
        if metrics.mount_point.is_some() {
            assert!(metrics.used_bytes.unwrap_or(0) <= metrics.total_bytes.unwrap_or(u64::MAX));
        }
    }
}
