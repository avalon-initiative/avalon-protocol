//! Avalon's network-facing service: identity, auth, social, guilds,
//! achievement verification, game registration.
//!
//! This is the one thing `hub` and `mobile-hub` are clients of — per the
//! decision that Hub is frontend-only and never becomes its own backend.
//! Games integrate against this service through `avalon-sdk`, not directly.
//!
//! No HTTP framework has been chosen yet; wiring that up, plus real
//! `avalon-chain`/`avalon-indexer` implementations, is the next milestone-1
//! task, not part of scaffolding.

#[tokio::main]
async fn main() {
    println!("avalon-server: scaffold only, not yet serving requests");
}
