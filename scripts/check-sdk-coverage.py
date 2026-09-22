#!/usr/bin/env python3
"""Issue #728: diffs `server`'s real, SDK-facing route table (`docs/generated/
openapi.json`, #723) against each SDK's (`bindings/csharp`, `bindings/ts`,
and Rust when checked out locally) actual HTTP call sites, and fails when the
server has an endpoint no SDK declares.

Issue #775 (epic #771) moved the Rust SDK's source to the `avalon-sdks`
repo — this script skips Rust coverage checking when `crates/sdk` isn't
present locally rather than reporting a false wall of missing routes; a
real cross-repo version of this check is future work, not part of #775.

This deliberately does NOT try to understand each SDK's method-naming
conventions (a Rust `friends()` vs. a C# `FriendsAsync()` vs. a TS
`listFriends()` all wrapping the same `GET /friends` tell us nothing useful
to compare by name). Instead it matches on the one thing all three SDKs share
regardless of naming: **HTTP method + path template**, normalized so that
`{id}` (server), `{}`/`{guild_id}` (Rust `format!`), `${guildId}` (TS
template literals), and `{guildId}` (C# interpolated strings) all collapse to
the same wildcard. Two path segments that are both "some dynamic value here"
are treated as equivalent without caring what either side calls it.

Per-language extraction, in order of preference within each SDK (see each
`extract_*` function's own comment for why):

- Rust (`crates/sdk/src/**/*.rs`): the account/auth domain (issue #724)
  calls through `crate::generated::paths::<tag>::<OPERATION_ID>` constants
  (build.rs-generated from the *whole* spec, not just the account domain —
  see `crates/sdk/build.rs`'s `generate_path_stubs`), which we resolve back
  to a real (method, path) pair using the same (tag, operationId) join the
  spec itself guarantees is unique. Every other domain (guilds, achievements,
  registry, schema/integrator-space, issuer registration, device/cross-node
  login, social, conversations) predates that migration and still calls
  `reqwest`'s `.get(format!(...))`/`.post(format!(...))`/etc. directly with a
  literal path template — we extract those too. `crates/sdk/src/http.rs` is
  excluded: its only such call sites are transport-layer unit tests hitting
  a mock server at a nonsense path ("/x"), not real endpoint declarations.
  `network.rs`/`managed_hosting.rs`/`sync_journal.rs`/`submission.rs` call
  ledger/node-internal routes that are already out of `openapi.json`'s scope
  entirely (its own `info.description` says so) — nothing to extract there.

- TypeScript (`bindings/ts/src/**/*.ts`, excluding `generated.ts` and
  `*.test.ts`): two call idioms, both going through `http.ts`'s `request()`
  eventually. Most `accountSession/*.ts` methods call `this.<verb>(path)`
  (`get`/`getQuery`/`post`/`postEmpty`/`postNoResponse`/`postEmptyNoResponse`/
  `patch`/`put`/`del`/`deleteWithBody`), inferring method from the verb name.
  A handful of free-standing functions (`crossNodeLogin.ts`, `recovery.ts`,
  `identityData.ts`, `integratorDirectory.ts`, `client.ts`,
  `integratorSession.ts`) call `request(serverUrl, path, { method: 'X', ... })`
  directly, with `'GET'` the implicit default when `method` is omitted
  (`http.ts`'s own `options.method ?? 'GET'`).

- C# (`bindings/csharp/AvalonSdk/*.cs`, excluding `AvalonSdk.Tests/`): two
  idioms. `AccountSession.*.cs` mostly calls typed helpers
  (`GetAsync`/`PostAsync`/`PutAsync`/`PatchAsync`/`DeleteAsync`, optionally
  suffixed `Json`) with a literal (possibly interpolated) path as the first
  string argument. Everything else (`Guilds.cs`, `Achievements.cs`,
  `Social.cs`, `Conversations.cs`, `CrossNodeLogin.cs`,
  `AccountSession.DeviceLogin.cs`, `AvalonClient.cs`) builds a raw
  `new HttpRequestMessage(HttpMethod.X, <literal-or-interpolated-url>)`
  directly. A couple of call sites build the URL in a local `var url = $"...";`
  one or two lines above the `HttpRequestMessage` call instead of inline —
  we track the most recently assigned `$"..."`/`"..."` literal per method
  body as a fallback for exactly that shape.

Any endpoint intentionally not exposed by a given SDK, confirmed as
deliberate (not just an extraction miss), gets a small allowlist entry below
— see `INTENTIONAL_GAPS`. Everything else the server has and an SDK's call
sites don't cover is a real, reportable gap.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
OPENAPI_PATH = REPO_ROOT / "docs" / "generated" / "openapi.json"

HTTP_METHODS = {"get", "post", "put", "patch", "delete"}


def normalize_path(path: str) -> str:
    """Collapse any dynamic path segment (`{id}`, `{}`, `${guildId}`,
    `{_guildId}`, `{Uri.EscapeDataString(handle)}`, ...) to `*`, drop any
    query string, and strip the leading `*` a call-site template picks up
    from interpolating its base URL in the same literal as the path
    (`"{}/guilds/..."`, `$"{ServerUrl}/guilds/..."`) — server paths never
    carry that prefix, so without stripping it every call-site path would
    spuriously fail to match."""
    path = path.split("?", 1)[0]
    path = re.sub(r"\$?\{[^{}]*\}", "*", path)
    if path.startswith("*"):
        path = path[1:]
    # A dynamic segment glued directly onto the end of a static segment with
    # no separating `/` (e.g. TS's `` `/guilds/discover${queryString}` ``,
    # where `queryString` is really a query-string suffix like `?limit=5`,
    # not a path segment) is a query string, not a path param — drop it,
    # the same way the literal-`?` split above handles the ordinary case.
    path = re.sub(r"(?<=[^/])\*$", "", path)
    path = re.sub(r"/{2,}", "/", path)
    return path.rstrip("/") or "/"


def load_openapi_routes() -> dict[tuple[str, str], dict]:
    """(method, normalized_path) -> {"method", "path", "operation_id", "tag"}."""
    doc = json.loads(OPENAPI_PATH.read_text())
    routes: dict[tuple[str, str], dict] = {}
    for path, methods in doc["paths"].items():
        for method, op in methods.items():
            if method not in HTTP_METHODS:
                continue
            key = (method.upper(), normalize_path(path))
            routes[key] = {
                "method": method.upper(),
                "path": path,
                "operation_id": op.get("operationId", "?"),
                "tag": (op.get("tags") or ["untagged"])[0],
            }
    return routes


def iter_files(root: Path, suffix: str, exclude_names: set[str] = frozenset(), exclude_dirs: set[str] = frozenset()):
    for p in sorted(root.rglob(f"*{suffix}")):
        if p.name in exclude_names:
            continue
        if any(part in exclude_dirs for part in p.parts):
            continue
        yield p


# --- Rust -------------------------------------------------------------------

RUST_TAG_OP_RE = re.compile(r"crate::generated::paths::(\w+)::(\w+)")
RUST_CALL_RE = re.compile(
    r'\.(get|post|put|patch|delete)\(\s*format!\(\s*"((?:[^"\\]|\\.)*)"', re.DOTALL
)


def extract_rust_calls(routes: dict[tuple[str, str], dict]) -> set[tuple[str, str]]:
    # (tag, OPERATION_ID_CONST) -> (method, normalized_path), derived from
    # the same spec the SDK's own build.rs draws its path constants from.
    by_tag_const: dict[tuple[str, str], tuple[str, str]] = {}
    for key, info in routes.items():
        const = info["operation_id"].upper()
        tag = info["tag"].replace("-", "_")
        by_tag_const[(tag, const)] = key

    sdk_src = REPO_ROOT / "crates" / "sdk" / "src"
    found: set[tuple[str, str]] = set()

    for f in iter_files(sdk_src, ".rs", exclude_names={"generated.rs", "http.rs"}):
        text = f.read_text()

        for tag, const in RUST_TAG_OP_RE.findall(text):
            key = by_tag_const.get((tag, const))
            if key:
                found.add(key)

        for method, literal in RUST_CALL_RE.findall(text):
            found.add((method.upper(), normalize_path(literal)))

    return found


# --- TypeScript ---------------------------------------------------------------

TS_VERB_TO_METHOD = {
    "get": "GET",
    "getQuery": "GET",
    "post": "POST",
    "postEmpty": "POST",
    "postNoResponse": "POST",
    "postEmptyNoResponse": "POST",
    "patch": "PATCH",
    "put": "PUT",
    "del": "DELETE",
    "deleteWithBody": "DELETE",
}
TS_THIS_CALL_RE = re.compile(
    r"this\.(" + "|".join(TS_VERB_TO_METHOD) + r")(?:<[^(]*?>)?\(\s*([`'\"])((?:[^\\]|\\.)*?)\2"
)
# request(serverUrl, path, { ... method: 'X' ... }) or request(serverUrl, path) (defaults to GET)
# `rest` is a small *bounded* lookahead window (not an open-ended search to
# the call's real closing paren) — an unbounded greedy tail here previously
# swallowed everything up to the next `);`/`.then`/`))` anywhere later in the
# file, which could skip right over several subsequent `request(...)` calls
# in one match.
TS_REQUEST_CALL_RE = re.compile(
    r"\brequest(?:<[^(]*?>)?\(\s*[^,]+,\s*([`'\"])((?:[^\\]|\\.)*?)\1(.{0,200})",
    re.DOTALL,
)
TS_METHOD_FIELD_RE = re.compile(r"method:\s*['\"](GET|POST|PUT|PATCH|DELETE)['\"]")


def extract_ts_calls() -> set[tuple[str, str]]:
    src = REPO_ROOT / "bindings" / "ts" / "src"
    found: set[tuple[str, str]] = set()

    for f in iter_files(src, ".ts", exclude_names={"generated.ts"}):
        if f.name.endswith(".test.ts"):
            continue
        text = f.read_text()

        for verb, _quote, literal in TS_THIS_CALL_RE.findall(text):
            method = TS_VERB_TO_METHOD[verb]
            found.add((method, normalize_path(literal)))

        for _quote, literal, rest in TS_REQUEST_CALL_RE.findall(text):
            # `rest` is a bounded lookahead of "whatever comes after the path
            # literal up to the call's closing paren" — search it (not the
            # whole file) for an explicit method, else default GET.
            m = TS_METHOD_FIELD_RE.search(rest[:300])
            method = m.group(1) if m else "GET"
            found.add((method, normalize_path(literal)))

    return found


# --- C# -----------------------------------------------------------------------

CSHARP_VERB_TO_METHOD = {
    "Get": "GET",
    "GetJson": "GET",
    "GetNullable": "GET",
    "GetQuery": "GET",
    "Post": "POST",
    "PostJson": "POST",
    "PostEmpty": "POST",
    "PostEmptyNoResponse": "POST",
    "PostNoResponse": "POST",
    "Put": "PUT",
    "Patch": "PATCH",
    "Delete": "DELETE",
    "DeleteWithBody": "DELETE",
}
# e.g. GetAsync<T>("/friends", ct) / PostJsonAsync<Req, Resp>($"/guilds/{id}", ...)
CSHARP_ASYNC_CALL_RE = re.compile(
    r"\b(" + "|".join(CSHARP_VERB_TO_METHOD) + r")Async(?:<[^(]*?>)?\(\s*\$?\"((?:[^\\\"]|\\.)*)\""
)
CSHARP_METHOD_TO_VERB = {
    "Get": "GET",
    "Post": "POST",
    "Put": "PUT",
    "Patch": "PATCH",
    "Delete": "DELETE",
}
CSHARP_HTTPREQUEST_RE = re.compile(
    r"new HttpRequestMessage\(\s*HttpMethod\.(" + "|".join(CSHARP_METHOD_TO_VERB) + r")\s*,\s*(.+?)\)",
    re.DOTALL,
)
CSHARP_STRING_LITERAL_RE = re.compile(r"\$?\"((?:[^\\\"]|\\.)*)\"")
CSHARP_URL_VAR_ASSIGN_RE = re.compile(r"var\s+url\s*=\s*\$?\"((?:[^\\\"]|\\.)*)\"")


def extract_csharp_calls() -> set[tuple[str, str]]:
    src = REPO_ROOT / "bindings" / "csharp" / "AvalonSdk"
    found: set[tuple[str, str]] = set()

    for f in iter_files(src, ".cs", exclude_dirs={"AvalonSdk.Tests", "bin", "obj"}):
        text = f.read_text()

        for verb, literal in CSHARP_ASYNC_CALL_RE.findall(text):
            found.add((CSHARP_VERB_TO_METHOD[verb], normalize_path(literal)))

        # `new HttpRequestMessage(HttpMethod.X, ...)` call sites sometimes
        # wrap the verb and the URL expression across two lines (e.g.
        # `Conversations.cs`'s multi-line calls) — matched against the whole
        # file (DOTALL) rather than line-by-line, which would silently miss
        # exactly those wrapped calls.
        url_var_assignments = [
            (m.start(), m.group(1)) for m in CSHARP_URL_VAR_ASSIGN_RE.finditer(text)
        ]

        for m in CSHARP_HTTPREQUEST_RE.finditer(text):
            verb, url_expr = m.groups()
            method = CSHARP_METHOD_TO_VERB[verb]
            lit = CSHARP_STRING_LITERAL_RE.search(url_expr)
            if lit:
                found.add((method, normalize_path(lit.group(1))))
            elif url_expr.strip() == "url":
                # Most recent `var url = $"...";` assignment before this call.
                candidates = [v for pos, v in url_var_assignments if pos < m.start()]
                if candidates:
                    found.add((method, normalize_path(candidates[-1])))

    return found


# --- Intentional, documented exclusions ---------------------------------------

# Each entry: (method, normalized_path) -> {"sdks": [...], "reason": "..."}.
# A route listed here is treated as covered for the named SDK(s) even though
# no call site was found for it, because it's a deliberate, documented
# design choice — not a gap this check should flag. Keep this list tiny and
# always cite the source that makes it "documented," not just "observed."
INTENTIONAL_GAPS: dict[tuple[str, str], dict] = {
    ("POST", "/identities/register/start"): {
        "sdks": ["csharp (bindings/csharp)"],
        "reason": (
            "WebAuthn registration ceremony — no .NET dependency in this SDK's "
            "stack does WebAuthn ceremony work at all, and its real audience "
            "(a Unity game binding a bearer token another surface already "
            "produced) never needs to run one itself. See "
            "bindings/csharp/AvalonSdk/AccountSession.cs's own header comment "
            "('Scoping decision')."
        ),
    },
    ("POST", "/identities/register/finish"): {
        "sdks": ["csharp (bindings/csharp)"],
        "reason": "Same WebAuthn-ceremony scoping decision as register/start above.",
    },
    ("POST", "/sessions/start"): {
        "sdks": ["csharp (bindings/csharp)"],
        "reason": (
            "WebAuthn login ceremony — same scoping decision as "
            "/identities/register/start above (AccountLogin is not ported "
            "for the same reason Register isn't)."
        ),
    },
    ("POST", "/sessions/finish"): {
        "sdks": ["csharp (bindings/csharp)"],
        "reason": "Same WebAuthn-ceremony scoping decision as sessions/start above.",
    },
    ("POST", "/me/passkeys/register/start"): {
        "sdks": ["csharp (bindings/csharp)"],
        "reason": (
            "Adding an additional passkey also drives a real WebAuthn "
            "registration ceremony — deliberately not ported, per "
            "bindings/csharp/AvalonSdk/AccountSession.Passkeys.cs's own header "
            "comment ('AddPasskeyAsync ... is deliberately not ported here')."
        ),
    },
    ("POST", "/me/passkeys/register/finish"): {
        "sdks": ["csharp (bindings/csharp)"],
        "reason": "Same add-passkey WebAuthn-ceremony scoping as register/start above.",
    },
}


def build_gap_lookup() -> dict[tuple[str, str], set[str]]:
    lookup: dict[tuple[str, str], set[str]] = {}
    for key, info in INTENTIONAL_GAPS.items():
        lookup[key] = set(info["sdks"])
    return lookup


# --- Main -----------------------------------------------------------------------


def main() -> int:
    routes = load_openapi_routes()

    sdk_coverage: dict[str, set[tuple[str, str]]] = {
        "typescript (bindings/ts)": extract_ts_calls(),
        "csharp (bindings/csharp)": extract_csharp_calls(),
    }
    # Issue #775 (epic #771): the Rust SDK physically moved to the avalon-sdks
    # repo, so its source isn't checked out here to scan. Skip it rather than
    # silently reporting every route as missing (an empty `rglob` on a
    # nonexistent directory yields nothing, not an error) — real Rust
    # coverage checking would need a cross-repo tool, not this script.
    if (REPO_ROOT / "crates" / "sdk").exists():
        sdk_coverage["rust (crates/sdk)"] = extract_rust_calls(routes)
    else:
        print(
            "skipping rust SDK coverage — crates/sdk moved to avalon-sdks (#775), "
            "not checked out in this repo"
        )
    gap_lookup = build_gap_lookup()

    missing: dict[str, list[dict]] = {name: [] for name in sdk_coverage}
    for key, info in sorted(routes.items(), key=lambda kv: (kv[1]["tag"], kv[1]["path"])):
        for sdk_name, covered in sdk_coverage.items():
            if key in covered:
                continue
            if sdk_name in gap_lookup.get(key, set()):
                continue
            missing[sdk_name].append(info)

    any_missing = any(missing.values())

    print(f"checked {len(routes)} SDK-facing routes from {OPENAPI_PATH.relative_to(REPO_ROOT)}")
    for sdk_name, entries in missing.items():
        if not entries:
            print(f"  OK   {sdk_name}: all routes covered")
            continue
        print(f"  FAIL {sdk_name}: {len(entries)} route(s) with no call site found")
        for info in entries:
            print(f"         {info['method']:6s} {info['path']}  (operationId: {info['operation_id']}, tag: {info['tag']})")

    if any_missing:
        print()
        print(
            "one or more SDKs are missing coverage for a server route above. "
            "Either add the missing call, or if the gap is genuinely intentional, "
            "add a documented entry to INTENTIONAL_GAPS in scripts/check-sdk-coverage.py."
        )
        return 1

    print()
    print("all SDKs cover every SDK-facing server route.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
