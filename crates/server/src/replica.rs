//! Replica-only mode (`AVALON_REPLICA_ONLY=true`): the node mirrors the
//! network, serves reads, relays and answers probes, but authors no shard.
//!
//! It holds no settlement signing key, never signs a tree head and needs no
//! registration. Any operation that would append a protocol event is refused.

use std::sync::atomic::{AtomicBool, Ordering};

/// `AppState::own_shard_id` of a replica-only node: never equal to a real shard id.
pub const NO_AUTHORED_SHARD: &str = "";

/// Marker carried by the database error `outbox::enqueue` raises in replica mode.
pub(crate) const REFUSAL_MARKER: &str = "replica-only node refuses to author protocol events";

static REPLICA_ONLY: AtomicBool = AtomicBool::new(false);

/// Marks this process as replica-only; called once at startup.
pub fn set_replica_only(value: bool) {
    REPLICA_ONLY.store(value, Ordering::SeqCst);
}

/// Whether this process is running replica-only.
pub fn is_replica_only() -> bool {
    REPLICA_ONLY.load(Ordering::SeqCst)
}

/// Whether `AVALON_REPLICA_ONLY` is set to a true value.
pub fn replica_only_from_env() -> bool {
    matches!(
        std::env::var("AVALON_REPLICA_ONLY")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

/// Refuses an authoring operation on a replica-only node.
pub fn require_authoring() -> Result<(), crate::error::AppError> {
    if is_replica_only() {
        Err(crate::error::AppError::ReplicaOnly)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_marker_maps_to_replica_only_error() {
        let err: crate::error::AppError = sqlx::Error::Protocol(REFUSAL_MARKER.to_string()).into();
        assert!(matches!(err, crate::error::AppError::ReplicaOnly));
        let other: crate::error::AppError = sqlx::Error::PoolClosed.into();
        assert!(matches!(other, crate::error::AppError::Database(_)));
    }

    #[test]
    fn authoring_is_refused_only_in_replica_mode() {
        let _g = crate::test_env::guard();
        set_replica_only(false);
        assert!(require_authoring().is_ok());
        set_replica_only(true);
        assert!(matches!(
            require_authoring(),
            Err(crate::error::AppError::ReplicaOnly)
        ));
        set_replica_only(false);
    }

    #[test]
    fn env_values() {
        let _g = crate::test_env::guard();
        std::env::remove_var("AVALON_REPLICA_ONLY");
        assert!(!replica_only_from_env());
        std::env::set_var("AVALON_REPLICA_ONLY", "TRUE");
        assert!(replica_only_from_env());
        std::env::set_var("AVALON_REPLICA_ONLY", "no");
        assert!(!replica_only_from_env());
        std::env::remove_var("AVALON_REPLICA_ONLY");
    }
}
