//! Avalon's network-facing service: identity, auth, social, guilds,
//! achievement verification, integrator registration.
//!
//! This is the one thing `hub` and `hub-app` are clients of — per the
//! decision that Hub is frontend-only and never becomes its own backend.
//! Integrators integrate against this service through `avalon-sdk`, not directly.
//!
//! Milestone 1, Epic: Identity & Player Profile — identity/auth endpoints
//! only. Social/guilds/achievements/integrations come with their own epics.
//!
//! The actual startup/run sequence lives in `avalon_server::run` — shared
//! with `src/bin/bundled_main.rs` (the `bundled-postgres`-feature
//! `avalon-server-bundled` binary), which starts a managed Postgres before
//! calling the same sequence.

#[tokio::main]
async fn main() {
    avalon_server::run::run().await;
}
