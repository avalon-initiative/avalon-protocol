//! Process-wide graceful shutdown: one SIGTERM/SIGINT flips a watch flag that the
//! listener and the background workers observe; a second signal, or the overall
//! deadline, exits immediately.

use std::time::Duration;

use tokio::sync::watch;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Cloneable view of the shutdown flag.
#[derive(Clone)]
pub struct Shutdown {
    rx: watch::Receiver<bool>,
}

impl Shutdown {
    /// A handle that never fires, for callers that run without signal handling.
    pub fn never() -> Self {
        let (tx, rx) = watch::channel(false);
        std::mem::forget(tx);
        Self { rx }
    }

    /// A handle plus the sender that triggers it.
    pub fn manual() -> (watch::Sender<bool>, Self) {
        let (tx, rx) = watch::channel(false);
        (tx, Self { rx })
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
}

/// Bound on each shutdown phase (connection drain, worker stop), from
/// `AVALON_SHUTDOWN_TIMEOUT_SECS`.
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

    async fn next(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.term.recv() => {}
            _ = self.int.recv() => {}
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.ok();
    }
}

/// Installs the signal handlers and returns the shutdown flag. Must be called
/// inside a tokio runtime.
pub fn install() -> Shutdown {
    let (tx, shutdown) = Shutdown::manual();
    let timeout = timeout_from_env();
    let mut signals = match Signals::new() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("could not install signal handlers, graceful shutdown disabled: {e}");
            return shutdown;
        }
    };
    tokio::spawn(async move {
        signals.next().await;
        tracing::info!("shutdown requested; draining (signal again to exit immediately)");
        let _ = tx.send(true);
        tokio::select! {
            _ = signals.next() => {
                tracing::warn!("second shutdown signal; exiting immediately");
            }
            _ = tokio::time::sleep(timeout * 2 + Duration::from_secs(1)) => {
                tracing::warn!("graceful shutdown exceeded its deadline; exiting");
            }
        }
        std::process::exit(1);
    });
    shutdown
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
}
