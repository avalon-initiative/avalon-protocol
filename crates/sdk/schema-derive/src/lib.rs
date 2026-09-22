//! `#[derive(AvalonSchema)]` (issue #386) — generates the `.proto`
//! message text and `default_visibility`/`field_visibility` maps an
//! Integrator Space schema publication needs from an ordinary Rust
//! struct. Re-exported through `avalon_sdk::schema`. See
//! `docs/architecture/sdk.md` and `integrator-space.md` for supported
//! field types, the visibility attributes, and field-numbering rules.

mod codegen;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(AvalonSchema, attributes(avalon))]
pub fn derive_avalon_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match codegen::derive_avalon_schema(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
