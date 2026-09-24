//! Public, read-only access to node topology and status data.
//!
//! Two pieces live here:
//!
//! - [`topology_public`]: the `AVALON_TOPOLOGY_PUBLIC` switch (default `true`).
//!   When `false`, the routes in [`router`] are not served at all (404).
//!   `/nodes/peers` and `/nodes/discover` are not part of that group and are
//!   unaffected by the switch.
//! - [`apply_cors`]: routes in [`PUBLIC_PATHS`] answer any origin
//!   (`Access-Control-Allow-Origin: *`, no credentials, `Content-Type` only).
//!   Every other route keeps the `AVALON_HUB_ORIGIN` allowlist. Scoping is by
//!   the explicit path list, never a wildcard.
//!
//! Extension point: new topology-style endpoints (topology, probe, trace) are
//! added with `.route(...)` inside [`router`]. Their paths are already in
//! [`PUBLIC_PATHS`]; a route on a new path must be added to that list too.

use axum::http::Method;
use axum::routing::{any_service, get};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::state::AppState;

/// Paths served with the permissive read-only CORS policy.
pub const PUBLIC_PATHS: &[&str] = &[
    "/nodes/status",
    "/nodes/peers",
    "/nodes/discover",
    "/ledger/sth/latest",
    "/ledger/mirror-progress",
    "/nodes/topology",
    "/nodes/probe",
    "/nodes/trace",
];

fn parse_flag(value: Option<&str>) -> bool {
    match value.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) => !matches!(v.as_str(), "false" | "0" | "no" | "off"),
        None => true,
    }
}

/// Whether the topology route group is served (`AVALON_TOPOLOGY_PUBLIC`,
/// default `true`; `false`/`0`/`no`/`off` disable it).
pub fn topology_public() -> bool {
    parse_flag(std::env::var("AVALON_TOPOLOGY_PUBLIC").ok().as_deref())
}

/// The topology route group. Add new routes here with `.route(...)`; the
/// group is only mounted when [`topology_public`] is true.
pub fn router() -> Router<AppState> {
    Router::new().route("/nodes/topology", get(crate::topology::topology))
}

/// Merges [`router`] into `base` when [`topology_public`] is true.
pub fn mount(base: Router<AppState>) -> Router<AppState> {
    mount_if(base, router(), topology_public())
}

fn mount_if<S: Clone + Send + Sync + 'static>(
    base: Router<S>,
    group: Router<S>,
    enabled: bool,
) -> Router<S> {
    if enabled {
        base.merge(group)
    } else {
        base
    }
}

fn public_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([axum::http::header::CONTENT_TYPE])
}

/// Wraps `app` so that [`PUBLIC_PATHS`] get the permissive CORS layer and
/// everything else gets `restricted`.
pub fn apply_cors(app: Router, restricted: CorsLayer) -> Router {
    let public = app.clone().layer(public_cors_layer());
    let mut out = Router::new().fallback_service(app.layer(restricted));
    for path in PUBLIC_PATHS {
        out = out.route(path, any_service(public.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, HeaderValue, Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn app() -> Router {
        let base: Router = Router::new()
            .route("/nodes/status", get(|| async { "ok" }))
            .route("/nodes/probe", post(|| async { "ok" }))
            .route("/secret", get(|| async { "ok" }));
        let restricted = CorsLayer::new()
            .allow_origin(vec![HeaderValue::from_static("http://hub.example")])
            .allow_methods([Method::GET])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);
        apply_cors(base, restricted)
    }

    async fn send(
        method: Method,
        path: &str,
        origin: &str,
        preflight: bool,
    ) -> axum::response::Response {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ORIGIN, origin);
        if preflight {
            b = b.header("access-control-request-method", "GET");
        }
        app().oneshot(b.body(Body::empty()).unwrap()).await.unwrap()
    }

    fn acao(r: &axum::response::Response) -> Option<&str> {
        r.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .map(|v| v.to_str().unwrap())
    }

    #[tokio::test]
    async fn public_routes_answer_any_origin() {
        let r = send(
            Method::GET,
            "/nodes/status",
            "http://anywhere.example",
            false,
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(acao(&r), Some("*"));
        assert!(r
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none());

        let r = send(
            Method::OPTIONS,
            "/nodes/probe",
            "http://anywhere.example",
            true,
        )
        .await;
        assert_eq!(acao(&r), Some("*"));
        let h = r
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .unwrap();
        assert_eq!(h.to_str().unwrap().to_ascii_lowercase(), "content-type");
    }

    #[tokio::test]
    async fn other_routes_keep_the_allowlist() {
        let r = send(Method::GET, "/secret", "http://anywhere.example", false).await;
        assert_eq!(acao(&r), None);
        let r = send(Method::OPTIONS, "/secret", "http://anywhere.example", true).await;
        assert_eq!(acao(&r), None);
        let r = send(Method::GET, "/secret", "http://hub.example", false).await;
        assert_eq!(acao(&r), Some("http://hub.example"));
    }

    #[test]
    fn flag_defaults_true_and_parses_false() {
        assert!(parse_flag(None));
        assert!(parse_flag(Some("true")));
        assert!(parse_flag(Some("")));
        assert!(!parse_flag(Some("false")));
        assert!(!parse_flag(Some(" FALSE ")));
        assert!(!parse_flag(Some("0")));
    }

    #[tokio::test]
    async fn mount_is_gated_by_the_flag() {
        let group: Router<()> = Router::new().route("/nodes/topology", get(|| async { "t" }));
        for (enabled, want) in [(true, StatusCode::OK), (false, StatusCode::NOT_FOUND)] {
            let app = mount_if(Router::<()>::new(), group.clone(), enabled);
            let r = app
                .oneshot(
                    Request::builder()
                        .uri("/nodes/topology")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), want);
        }
    }
}
