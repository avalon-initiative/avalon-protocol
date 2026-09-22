//! Wire-shape types generated from `docs/generated/openapi.json` by
//! `build.rs`. Do not hand-edit — see `build.rs`'s `SCHEMA_NAMES` for the
//! current coverage and why some request/response types are excluded.
//!
//! `missing_docs` is allowed here (unlike everywhere else in this crate):
//! what documentation these types carry comes from the schema's own
//! `description` fields, and an enum variant generated from a JSON
//! `enum: [...]` list has nowhere for a doc comment to come from. The
//! domain-shaped re-exports in `crate::types` are where the prose lives.
#![allow(dead_code, missing_docs, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/generated.rs"));
