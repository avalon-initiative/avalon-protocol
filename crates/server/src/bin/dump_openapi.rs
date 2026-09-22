//! Issue #723: regenerates the checked-in OpenAPI schema from the real,
//! annotated handler/type definitions — `make openapi` writes this
//! command's stdout to `docs/generated/openapi.json`.

use avalon_server::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() {
    println!(
        "{}",
        ApiDoc::openapi()
            .to_pretty_json()
            .expect("ApiDoc always serializes")
    );
}
