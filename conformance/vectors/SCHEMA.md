# Conformance vector format

Shared, canonical test vectors for cross-SDK conformance — client-side
"smart" behavior that generic wire-shape codegen does not exercise. One
JSON file per behavior. Consumed by a thin test-runner in each SDK in the
`avalon-sdks` repository (`languages/rust`, `languages/csharp`, `languages/typescript`).

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

- `supportedIn` lists which SDKs' conformance tests exercise this vector today. A language missing from it either lacks the behavior or lacks the test; `notSupported` says which.
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

- `cross-node-login.json` — `CrossNodeLoginGrant` signing: the
  deterministic, offline-testable core of the cross-device/cross-node
  login path. Supported in all three SDKs.
- `session-continuation.json` — `ContinuationToken` minting, the
  primitive behind the Hub's automatic reconnect-on-401. TypeScript only
  today (`languages/typescript/src/crypto/continuation.ts`); Rust and C# have no
  client-side implementation despite `crates/protocol` defining the same
  signing-bytes contract server-side.
- `websocket-interest-claim.json` — `InterestClaim` minting, the
  signed payload an `AccountSession`-level websocket subscribe sends once
  it receives the server's `node_info` hello. TypeScript only
  today (`languages/typescript/src/accountSession/realtime.ts` +
  `crypto/interestClaim.ts`).
- `bip39-mnemonic.json` — BIP39 recovery-phrase-derived signing keys.
  TypeScript only today (`languages/typescript/src/crypto/mnemonic.ts`).
- `attestation-signing.json` — attestation issuance, bulk issuance, and
  revocation signing bytes. Supported in all three SDKs, each with its own
  hand-written construction. This file is load-bearing: the Rust SDK no
  longer calls the server's own signing-byte functions directly, so
  nothing but these vectors keeps client and server producing the same
  bytes.
- `signed-tree-head.json` — the Signed Tree Head signing message and
  signature. Supported in all three SDKs, each with its own hand-written
  construction, verified against pinned trust anchors.
- `witness-cosigned-tree-head.json` — witness-cosigned tree head
  acceptance (`avalon_protocol::cosigned_sth::verify_cosigned_tree_head`):
  accepted, below-threshold, unknown-witness, stale, conflicting-heads
  (equivocation) and author-as-known-witness cases (the author's own valid
  signature counts as the vote of a known witness holding its key), all against precomputed signatures under one fixed
  set of author/witness key seeds. Rust only today — production
  known-list management and cosignature gossip (#946/#947) haven't landed
  in any SDK yet.
- `identity-chain.json` — per-identity event chains
  (`avalon_protocol::identity_chain`). Unlike the files above it has two
  arrays instead of `vectors`: `hashVectors` (`compute_event_hash` inputs —
  `kind`, `issuer`, `subject`, `payloadJson`, `timestampNanos` as a decimal
  string, `seq`, `prevHashHex` — and the `expectedHashHex` sha256 they must
  produce) and `resolutionCases` (a set of `events` with `label`, `seq`,
  `prevLabel`, `class` of `ordinary`/`monotonic`/`critical`,
  `timestampSeconds` and a fixed `hashHex`; `expected.acceptedLabels` is the
  resolved chain in order and `expected.forkedAtSeq` the fork position or
  null). Every ordering of a case's events must resolve identically. Rust
  only today.

## Both sides of the wire

Unlike the four client-behavior vectors above, `attestation-signing.json`,
`signed-tree-head.json`, `witness-cosigned-tree-head.json`, `identity-chain.json`,
`cross-node-login.json`, `session-continuation.json`, and
`websocket-interest-claim.json` also describe something the *server*
verifies. `crates/protocol/tests/conformance.rs`
asserts `avalon-protocol`'s own implementations against those same files, so
a format change fails a test whichever side moves first — an SDK's runner if
the SDK drifts, the protocol runner if the server does.

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
