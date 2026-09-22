# Conformance vector format

Shared, canonical test vectors for cross-SDK conformance (issue #727, epic
#722) — client-side "smart" behavior that generic wire-shape codegen
(#723-#726) does not exercise. One JSON file per behavior. Consumed by a
thin test-runner in each of `crates/sdk`, `bindings/csharp`, `bindings/ts`.

Each file has this shape:

```json
{
  "description": "prose describing what this behavior is and why it matters",
  "supportedIn": ["rust", "csharp", "typescript"],
  "notSupported": {
    "<language not in supportedIn>": "why — a real gap, not a TODO"
  },
  "vectors": [
    { "name": "...", "input": { ... }, "expected": { ... } }
  ]
}
```

- `supportedIn` lists which SDKs actually implement this behavior today.
  A runner only asserts full pass/fail for its own language when it's
  listed here. When a language is *not* listed, its runner must not
  fabricate a passing implementation — it records the vector as an
  explicit, visible skip (still a green test run, but the message names
  the gap) and points at `notSupported.<language>` for why.
- `input`/`expected` fields are behavior-specific (see each file's own
  keys) but always plain JSON-representable values — no language-specific
  types. Byte strings are lowercase hex (`...Hex` suffix) or exact UTF-8
  text (`...Utf8` suffix); timestamps are given as both Unix seconds and
  RFC 3339 so each SDK can use whichever its own time type prefers.
- All Ed25519 signing vectors share one fixed, non-secret test keypair
  seed per file (`signingKeySeedHex`) — never a real credential.

## Files

- `cross-node-login.json` — `CrossNodeLoginGrant` signing (#623/#707): the
  deterministic, offline-testable core of the cross-device/cross-node
  login path. Supported in all three SDKs.
- `session-continuation.json` — `ContinuationToken` minting (#525), the
  primitive behind #712's automatic reconnect-on-401. TypeScript only
  today (`bindings/ts/src/crypto/continuation.ts`); Rust and C# have no
  client-side implementation despite `crates/protocol` defining the same
  signing-bytes contract server-side.
- `websocket-interest-claim.json` — `InterestClaim` minting (#610), the
  signed payload an `AccountSession`-level websocket subscribe sends once
  it receives the server's `node_info` hello (#712). TypeScript only
  today (`bindings/ts/src/accountSession/realtime.ts` +
  `crypto/interestClaim.ts`).
- `bip39-mnemonic.json` — BIP39 recovery-phrase-derived signing keys
  (#134/#712). TypeScript only today
  (`bindings/ts/src/crypto/mnemonic.ts`).

## Standing requirement

Whenever a new "smart client" behavior (anything beyond a generic wire-
shape request/response — a local derivation, a signed local assertion, a
multi-step handshake, a reconnect/retry rule) is added to *any* one SDK,
add a vector file here for it in the same change, even if the other two
SDKs don't implement it yet — list the implementing SDK(s) in
`supportedIn` and document the others under `notSupported`. Do not let a
behavior like this exist in only one SDK's tests; the whole point of this
directory is one shared source of truth all three consume, so a gap is
visible in vector form (via `notSupported`) the moment it's found, not
discovered again later the hard way.
