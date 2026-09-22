//! Domain/wire types this SDK owns outright (issue #774, epic #771).
//!
//! These used to be re-used straight out of `avalon-protocol`, which made
//! the Rust SDK a special case among the three SDKs: the C# and TypeScript
//! SDKs have always carried their own definitions, derived from the
//! published schema (`docs/generated/openapi.json`) plus the documented
//! wire format. This module is the Rust equivalent, so an integrator
//! depending on `avalon-sdk` links a client library, not the server's own
//! domain crate.
//!
//! Two kinds of type live here:
//!
//! - **Schema-generated** — [`identity::Genre`], [`social::PresenceStatus`],
//!   [`guilds::GuildLink`], [`guilds::RoleBadge`] (and its
//!   `RoleBadgeIcon`/`RoleBadgeColor` halves) are generated fresh from
//!   `docs/generated/openapi.json` by `build.rs` and simply re-exported
//!   here under a domain-shaped path, so a caller writing
//!   `avalon_sdk::types::social::PresenceStatus` doesn't have to know or
//!   care which types happen to be codegen'd.
//! - **Hand-written** — everything else: the small structs/enums the
//!   published schema doesn't describe on its own (an id newtype, a
//!   capability string vocabulary, the ledger event shapes a
//!   managed-hosting client submits). Each carries the same wire format
//!   the server produces; that agreement is a documented contract now,
//!   not a compiler-enforced one, and the conformance suite
//!   (`conformance/vectors/`, issue #727) is what keeps the signed parts
//!   of it honest.

pub mod events;
pub mod guilds;
pub mod identity;
pub mod ids;
pub mod integrators;
pub mod permissions;
pub mod social;
