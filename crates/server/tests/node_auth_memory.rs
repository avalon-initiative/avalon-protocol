//! Measures the replay cache's heap use with a counting allocator: bytes per live nonce at a
//! few sizes and at the ceiling, and that the memory comes back once the entries expire.
//! Run with `--nocapture` to see the numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use avalon_server::node_auth::{NodeAuth, MAX_REPLAY_ENTRIES};
use avalon_server::nodes::PeerTable;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const NOW: i64 = 1_790_000_000;

#[test]
fn the_replay_cache_memory_is_measured_at_the_ceiling_and_comes_back_when_idle() {
    let auth = NodeAuth::with_limits(
        PeerTable::new(),
        "net",
        Some("own"),
        None,
        3000,
        MAX_REPLAY_ENTRIES,
    );
    let base = LIVE.load(Ordering::Relaxed);
    let mut worst_per_entry = 0.0f64;
    for target in [10_000usize, 100_000, MAX_REPLAY_ENTRIES] {
        auth.fill_replay_cache_for_measurement(target - auth.replay_len(), NOW);
        let used = LIVE.load(Ordering::Relaxed) - base;
        let per_entry = used as f64 / auth.replay_len() as f64;
        eprintln!(
            "{} live nonces: {:.1} MB, {:.1} bytes per nonce",
            auth.replay_len(),
            used as f64 / 1e6,
            per_entry
        );
        worst_per_entry = worst_per_entry.max(per_entry);
    }
    assert_eq!(auth.replay_len(), MAX_REPLAY_ENTRIES);
    let at_ceiling = LIVE.load(Ordering::Relaxed) - base;
    // The bound the hosting docs state.
    assert!(
        at_ceiling < 100 * 1024 * 1024,
        "{at_ceiling} bytes at the ceiling"
    );
    assert!(worst_per_entry < 110.0);

    // A request long after the retention window expires everything and frees the memory.
    auth.fill_replay_cache_for_measurement(1, NOW + 1000);
    assert_eq!(auth.replay_len(), 1);
    let idle = LIVE.load(Ordering::Relaxed) - base;
    eprintln!("after expiry: {} bytes", idle);
    assert!(
        idle < 1024 * 1024,
        "{idle} bytes held after the cache emptied"
    );
}
