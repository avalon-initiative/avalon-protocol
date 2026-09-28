//! Process-wide graceful shutdown.
//!
//! The first SIGTERM/SIGINT while the process is still starting exits promptly with the
//! conventional `128 + signal` status; once the node is serving it flips a watch flag that the
//! listener, the websocket sessions and the workers observe. A second signal, or the deadline
//! for the serve and worker phases, exits with status 1. The final stop of a managed database
//! runs after the deadline is disarmed. Exits go through `process::exit`, so `atexit` hooks
//! (used to terminate a managed database) always run.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{watch, Notify};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const STOPPING: u8 = 2;

struct Inner {
    phase: AtomicU8,
}

/// Cloneable view of the shutdown flag and phase.
#[derive(Clone)]
pub struct Shutdown {
    rx: watch::Receiver<bool>,
    inner: Arc<Inner>,
}

impl Shutdown {
    fn with_phase(phase: u8) -> (watch::Sender<bool>, Self) {
        let (tx, rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            phase: AtomicU8::new(phase),
        });
        (tx, Self { rx, inner })
    }

    /// A handle that never fires, for callers that run without signal handling.
    pub fn never() -> Self {
        let (tx, shutdown) = Self::with_phase(RUNNING);
        std::mem::forget(tx);
        shutdown
    }

    /// A handle plus the sender that triggers it, already in the serving phase.
    pub fn manual() -> (watch::Sender<bool>, Self) {
        Self::with_phase(RUNNING)
    }

    pub fn is_requested(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves once shutdown has been requested.
    pub async fn requested(&mut self) {
        while !*self.rx.borrow_and_update() {
            if self.rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Start-up is over: a first signal now drains instead of exiting immediately.
    pub fn mark_running(&self) {
        let _ = self.inner.phase.compare_exchange(
            STARTING,
            RUNNING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    /// The serve and worker phases are done: the deadline no longer fires, so what follows
    /// (stopping a managed database) is bounded by its own timeout.
    pub fn begin_final_stop(&self) {
        self.inner.phase.store(STOPPING, Ordering::SeqCst);
    }
}

/// Bound on each of the drain and worker-stop phases, and on stopping a managed database,
/// from `AVALON_SHUTDOWN_TIMEOUT_SECS` (read when used, so it must be called after `.env` is
/// loaded).
pub fn timeout_from_env() -> Duration {
    parse_timeout(
        std::env::var("AVALON_SHUTDOWN_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

fn parse_timeout(raw: Option<&str>) -> Duration {
    let secs = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Deadline after the first signal for the serve and worker phases together.
fn drain_deadline(timeout: Duration) -> Duration {
    timeout * 2 + Duration::from_secs(1)
}

/// SIGTERM and SIGINT listeners (Ctrl-C only on non-unix).
struct Signals {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
}

impl Signals {
    fn new() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                term: signal(SignalKind::terminate())?,
                int: signal(SignalKind::interrupt())?,
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    /// Waits for the next signal and returns its number.
    async fn next(&mut self) -> i32 {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.term.recv() => 15,
                _ = self.int.recv() => 2,
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
            2
        }
    }
}

static GLOBAL: OnceLock<Shutdown> = OnceLock::new();

/// Installs the signal handlers and returns the shutdown handle, in the starting phase. Must
/// be called inside a tokio runtime, before any start-up work worth interrupting.
pub fn install() -> Shutdown {
    let (tx, shutdown) = Shutdown::with_phase(STARTING);
    let mut signals = match Signals::new() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("could not install signal handlers, graceful shutdown disabled: {e}");
            return shutdown;
        }
    };
    let _ = GLOBAL.set(shutdown.clone());
    let handle = shutdown.clone();
    tokio::spawn(async move {
        let signo = signals.next().await;
        if handle.inner.phase.load(Ordering::SeqCst) == STARTING {
            tracing::warn!("signal received during start-up; exiting");
            std::process::exit(128 + signo);
        }
        tracing::info!("shutdown requested; draining (signal again to exit immediately)");
        let _ = tx.send(true);
        let deadline = tokio::time::sleep(drain_deadline(timeout_from_env()));
        tokio::pin!(deadline);
        let mut deadline_armed = true;
        loop {
            tokio::select! {
                _ = signals.next() => {
                    tracing::warn!("second shutdown signal; exiting immediately");
                    break;
                }
                _ = &mut deadline, if deadline_armed => {
                    if handle.inner.phase.load(Ordering::SeqCst) == STOPPING {
                        deadline_armed = false;
                        continue;
                    }
                    tracing::warn!("graceful shutdown exceeded its deadline; exiting");
                    break;
                }
            }
        }
        std::process::exit(1);
    });
    shutdown
}

static OPEN_SESSIONS: AtomicUsize = AtomicUsize::new(0);
static SESSIONS_CLOSED: Notify = Notify::const_new();

/// One live websocket session: counted so the drain waits for it, and told when to close.
pub struct SocketSession {
    shutdown: Shutdown,
}

impl SocketSession {
    /// Resolves once the server wants the session closed.
    pub async fn closing(&mut self) {
        self.shutdown.requested().await;
    }
}

impl Drop for SocketSession {
    fn drop(&mut self) {
        if OPEN_SESSIONS.fetch_sub(1, Ordering::SeqCst) == 1 {
            SESSIONS_CLOSED.notify_waiters();
        }
    }
}

/// Registers a websocket session against the process-wide shutdown handle.
pub fn socket_session() -> SocketSession {
    socket_session_with(GLOBAL.get().cloned().unwrap_or_else(Shutdown::never))
}

/// As [`socket_session`], against an explicit handle.
pub fn socket_session_with(shutdown: Shutdown) -> SocketSession {
    OPEN_SESSIONS.fetch_add(1, Ordering::SeqCst);
    SocketSession { shutdown }
}

/// Resolves when no websocket session is open.
pub async fn sessions_closed() {
    loop {
        let notified = SESSIONS_CLOSED.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if OPEN_SESSIONS.load(Ordering::SeqCst) == 0 {
            return;
        }
        notified.await;
    }
}

/// Sends a close frame (1001, going away) to the peer of an upgraded connection.
pub async fn send_going_away(socket: &mut axum::extract::ws::WebSocket) {
    use axum::extract::ws::{CloseFrame, Message};
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 1001,
            reason: "server shutting down".into(),
        })))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_defaults_and_overrides() {
        assert_eq!(parse_timeout(None), Duration::from_secs(30));
        assert_eq!(parse_timeout(Some("5")), Duration::from_secs(5));
        assert_eq!(parse_timeout(Some("0")), Duration::from_secs(30));
        assert_eq!(parse_timeout(Some("abc")), Duration::from_secs(30));
    }

    #[test]
    fn drain_deadline_is_twice_the_timeout_plus_a_second() {
        assert_eq!(
            drain_deadline(Duration::from_secs(5)),
            Duration::from_secs(11)
        );
    }

    #[tokio::test]
    async fn requested_resolves_after_trigger() {
        let (tx, mut shutdown) = Shutdown::manual();
        assert!(!shutdown.is_requested());
        let waiter = tokio::spawn(async move {
            shutdown.requested().await;
        });
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn never_does_not_resolve() {
        let mut shutdown = Shutdown::never();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), shutdown.requested())
                .await
                .is_err()
        );
    }

    #[test]
    fn phases_move_starting_running_stopping() {
        let (_tx, s) = Shutdown::with_phase(STARTING);
        assert_eq!(s.inner.phase.load(Ordering::SeqCst), STARTING);
        s.mark_running();
        assert_eq!(s.inner.phase.load(Ordering::SeqCst), RUNNING);
        s.begin_final_stop();
        assert_eq!(s.inner.phase.load(Ordering::SeqCst), STOPPING);
        s.mark_running();
        assert_eq!(
            s.inner.phase.load(Ordering::SeqCst),
            STOPPING,
            "mark_running must not undo the final stop"
        );
    }
}
