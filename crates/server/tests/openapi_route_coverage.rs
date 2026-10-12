//! Every route mounted in `lib.rs` and `topology_access.rs` is in the OpenAPI document or on the
//! reviewed allowlist below, and every documented operation is mounted (#1388).
//!
//! Axum cannot list its routes, so the mounted set is read from the `.route("path", get(..).post(..))`
//! calls in the router source. A route with a non-literal path is not seen; keep paths literal.

use std::collections::BTreeSet;

use avalon_server::openapi::ApiDoc;
use utoipa::OpenApi;

const ROUTER_SOURCES: [&str; 2] = [
    include_str!("../src/lib.rs"),
    include_str!("../src/topology_access.rs"),
];

const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// Mounted on purpose but outside the spec, each with its reason. Remove an entry when the route is documented.
const NOT_DOCUMENTED: &[(&str, &str, &str)] = &[
    (
        "post",
        "/internal/indexer/apply",
        "internal: indexer role to server, shared secret",
    ),
    (
        "post",
        "/internal/indexer/rebuild",
        "internal: indexer role to server, shared secret",
    ),
    (
        "get",
        "/ledger/cross-shard-root",
        "node-to-node ledger read",
    ),
    (
        "post",
        "/ledger/finalize-batch",
        "managed-hosting remote signing, hand-written in the SDKs",
    ),
    (
        "get",
        "/ledger/mirror-progress",
        "node-to-node retention check",
    ),
    (
        "post",
        "/ledger/prepare-batch",
        "managed-hosting remote signing, hand-written in the SDKs",
    ),
    (
        "get",
        "/ledger/proof/consistency",
        "tree-head read, hand-written in the SDKs and pinned by vectors",
    ),
    (
        "get",
        "/ledger/proof/inclusion",
        "tree-head read, hand-written in the SDKs and pinned by vectors",
    ),
    (
        "get",
        "/ledger/remote-submit-status",
        "node-to-node remote signing status",
    ),
    (
        "get",
        "/ledger/shard-family",
        "node-to-node shard discovery",
    ),
    (
        "get",
        "/ledger/sth/latest",
        "tree-head read, hand-written in the SDKs and pinned by vectors",
    ),
    (
        "get",
        "/ledger/sth/{tree_size}",
        "tree-head read, hand-written in the SDKs and pinned by vectors",
    ),
    (
        "post",
        "/ledger/submit",
        "node-to-node write, bearer authenticated",
    ),
    ("post", "/mirror/notify", "node-to-node mirror push"),
    ("post", "/nodes/announce", "node-to-node gossip"),
    (
        "get",
        "/nodes/discover",
        "node discovery, hand-written in the SDKs",
    ),
    ("get", "/nodes/log-level", "operator runtime control"),
    ("post", "/nodes/log-level", "operator runtime control"),
    ("get", "/nodes/peers", "node-to-node peer list"),
    ("post", "/nodes/relay", "node-to-node relay"),
    (
        "post",
        "/nodes/replicate-chat",
        "node-to-node chat replication",
    ),
    (
        "get",
        "/nodes/status",
        "node status, hand-written in the SDKs",
    ),
    (
        "get",
        "/registry/{slug}",
        "public registry page served as a document, not JSON API",
    ),
    ("get", "/secret", "node-to-node shared-secret probe"),
    (
        "get",
        "/ws/messages",
        "websocket upgrade, not expressible as an operation",
    ),
    (
        "get",
        "/ws/presence",
        "websocket upgrade, not expressible as an operation",
    ),
];

type Operation = (String, String);

fn mounted() -> BTreeSet<Operation> {
    let mut found = BTreeSet::new();
    for source in ROUTER_SOURCES {
        let mut rest = source;
        while let Some(at) = rest.find(".route(") {
            let args = &rest[at + ".route(".len()..];
            let Some(path) = args
                .trim_start()
                .strip_prefix('"')
                .and_then(|a| a.split_once('"'))
            else {
                rest = args;
                continue;
            };
            let mut depth = 1usize;
            let end = args
                .char_indices()
                .find(|&(_, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })
                .map_or(args.len(), |(i, _)| i);
            let after_path = &args[args.find(path.0).unwrap() + path.0.len() + 1..end];
            for method in METHODS {
                if after_path.contains(&format!("{method}(")) {
                    found.insert((method.to_string(), path.0.to_string()));
                }
            }
            rest = &args[end..];
        }
    }
    found
}

fn documented() -> BTreeSet<Operation> {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let mut found = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in METHODS {
            if item.get(method).is_some() {
                found.insert((method.to_string(), path.clone()));
            }
        }
    }
    found
}

fn allowlist() -> BTreeSet<Operation> {
    NOT_DOCUMENTED
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect()
}

#[test]
fn the_route_scan_finds_the_router() {
    assert!(
        mounted().len() > 150,
        "route scan found {} routes",
        mounted().len()
    );
}

#[test]
fn every_mounted_route_is_documented_or_allowlisted() {
    let (mounted, documented, allowed) = (mounted(), documented(), allowlist());
    let missing: Vec<_> = mounted
        .iter()
        .filter(|op| !documented.contains(*op) && !allowed.contains(*op))
        .collect();
    assert!(
        missing.is_empty(),
        "mounted but neither in the OpenAPI document nor in NOT_DOCUMENTED: {missing:?}"
    );
}

#[test]
fn every_documented_operation_is_mounted() {
    let (mounted, documented) = (mounted(), documented());
    let ghosts: Vec<_> = documented.difference(&mounted).collect();
    assert!(ghosts.is_empty(), "documented but not mounted: {ghosts:?}");
}

#[test]
fn the_allowlist_has_no_stale_entries() {
    let (mounted, documented, allowed) = (mounted(), documented(), allowlist());
    let stale: Vec<_> = allowed
        .iter()
        .filter(|op| !mounted.contains(*op) || documented.contains(*op))
        .collect();
    assert!(
        stale.is_empty(),
        "NOT_DOCUMENTED lists routes that are unmounted or already documented: {stale:?}"
    );
}
