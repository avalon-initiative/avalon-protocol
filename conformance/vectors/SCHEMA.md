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
  set of author/witness key seeds. Asserted by all three SDKs against
  an explicit, caller-supplied known list; building that list from
  discovery is not covered by this file.
- `witness-announce.json` — witness advert proofs
  (`avalon_protocol::witness::verify_witness_announce`). A witness advertises a
  base url with a proof: an Ed25519 signature by the advertised key over
  `avalon-witness-announce-v1`, a u32-BE length and the base url bytes, a
  u32-BE length and the key id string bytes, and `announced_at` as unix seconds
  big-endian i64. Accepted only when the key id is a 32-byte hex key, the proof
  is a 64-byte hex signature that verifies under it, and `announced_at` is within
  one hour of the verifier's clock (inclusive, either direction). Each vector's
  `input.messageHex` is the exact signed message where the key id is hex.
  Signatures come from the same fixed seeds as the cosigned head vectors.
- `node-request.json` — node-to-node request credential
  (`avalon_protocol::node_request::verify_node_request_header`). The
  `x-avalon-node-auth` header carries an Ed25519 signature over
  `avalon-node-request-v1` and u32-BE length-prefixed method, path, sha256 of
  the body, network id, recipient and signer peer id, then the timestamp as
  big-endian i64 and the 16 nonce bytes. The header also carries that body hash
  as `bh` (64 lowercase hex, between `nonce` and `sig`): a receiver verifies the
  signature over the message built from the header's own `bh` first, without
  reading the body, and only then requires sha256(body) to equal `bh`
  (`body_hash` otherwise, last in `checkOrder`). `method` is the uppercase HTTP method
  string; `path` is the raw wire request-target path (no query or fragment, no
  decoding or normalisation, so `%2F` stays `%2F`), and a non-HTTP stream form
  must sign exactly the same method and path strings. Signable paths start with
  `/`, use only bytes `0x21`-`0x7e` other than `\`, `?` and `#`, and have no `//`
  and no `.` or `..` segment. Network id and every recipient are non-empty, the
  whole accepted-recipient list is validated before any signature check, and a
  negative skew is an error. Signature verification is strict Ed25519.
  Every vector with a non-null `expected.error` must be rejected with exactly
  that code (`rejectionCodes`; `checkOrder` gives precedence). `bodyHex` and
  `bodySha256Hex` express non-UTF-8 bodies; `messageHex` is the exact message a
  header is verified over for `signingRecipient`; `signedBySeed` vectors are
  reproduced byte for byte by signing with the file's seed. The file is
  generated by `crates/protocol/tests/node_request_vectors.rs`
  (`AVALON_REGEN_NODE_REQUEST_VECTORS=1 cargo test -p avalon-protocol --test
  node_request_vectors`), which fails when the committed file differs.
  Replay protection is the receiver's job and not covered by vectors: key a cache
  on `(peer id, nonce)` regardless of which recipient matched, keep entries at
  least twice the skew (a timestamp of `now + skew` stays valid until
  `now + 2 * skew`), and insert only after the signature, the PeerId-from-key
  check and the standing checks pass. The receiver must derive the libp2p PeerId
  from `key` and compare it to `peer` before granting standing; nonces are 16
  random bytes from a CSPRNG. No SDK implements it (node-only routes, not in
  OpenAPI): `supportedIn` is empty.
- `self-certifying-tree-head.json` — verification of a tree head of a
  self-certifying `node:<sha256-of-key>` shard
  (`avalon_protocol::shard_identity`). Each vector's `input` has `shardId`, an
  optional `signingPublicKeyHex` (absent means the server presented none) and a
  `head` (`treeSize`, `rootHashHex`, `networkId`, `signingKeyId`,
  `createdAtUnixSeconds`, `createdAtRfc3339`, `signatureHex`; `treeSize` is a JSON
  number, or a decimal string when its magnitude is 2^53 or more, and
  `createdAtUnixSeconds` is always the floor of `createdAtRfc3339`). `expected.check`
  is which verification applies to the id (`self_certifying`, `core_network` or
  `unsupported`), `expected.verified` the outcome and `expected.failure` the first
  failing check, in this order: `not_self_certifying` (the id is not a valid
  `node:` id), `missing_key`, `malformed_key` (not exactly 64 lowercase hex
  characters decoding to a canonical Ed25519 point that is not of small order: y at
  or above 2^255-19, the identity, and every point of order 2, 4 or 8, including
  x = 0 with the sign bit set, are rejected; a point with a torsion component that
  is neither small-order nor non-canonical, such as the y = 3 vector, is deliberately
  accepted, so an SDK must not add a prime-order-subgroup-only rule), `key_id_mismatch` (sha256 of the
  key bytes differs from the id's hash), `bad_signature` (the key's signature does
  not verify over the standard Signed Tree Head signing bytes). Signature
  verification is cofactorless: S must be below the group order L, and the point
  recomputed as [S]B - [k]A must equal the signature's R byte for byte, so a
  non-canonical R and an R with a torsion component both fail, while a canonical
  identity R with S = k*a verifies. The signature hex is decoded like Rust
  `hex::decode`: even length, digits 0-9, a-f and A-F only (upper and mixed case
  are accepted), no sign, `0x` prefix or whitespace. The id and the key are
  matched as exactly lowercase hex with no surrounding characters (a trailing
  newline or NUL is a failure). The file's
  `generation` field says how the two fixed keys and every signature were
  produced. Supported in all three SDKs.
- `shard-sibling-routing.json` — routing a write to one of an owner's sibling shards by a caller-supplied key
  (`shard_family::route_write`, `ShardFamily.RouteWrite`, `routeWrite`). Client-side only, so it originates here and has no
  protocol-side runner. Rendezvous hashing: the weight of a candidate is SHA-256 of `avalon-shard-route-v1` (UTF-8, no
  length) then owner, key and shard id, each as a u32 big-endian UTF-8 byte length and the bytes; the greatest weight
  (32 big-endian bytes) wins, ties go to the bytewise smaller shard id, candidates are first reduced to unique members of
  the owner's family and none left means no route (`shardId` null). Three arrays instead of `vectors`. `weightVectors`:
  `input` `owner`, `key`, `shardId`; `expected` `preimageHex`, `weightHex`. `routeVectors`: `input` `owner`, `key`,
  `siblings`; `expected.shardId` (stability, order and duplicates, non-members, empty set, Unicode keys, tie-break).
  `movementVectors`: `input` `owner`, `keys`, `before`, `after` sibling sets; `expected` `before` and `after` route per key
  (a key only moves onto an added sibling or off a removed one; the runners assert that too). No cross-sibling atomicity.
  Supported in all three SDKs.
- `known-list-selection.json` — the client's known-list rules
  (`avalon_protocol::client_known_list`). It has two arrays instead of
  `vectors`. `prefixVectors`: the diversity prefix derived from a node base url
  with no DNS lookup (`v4:a.b.c.0/24`, `v6:g1:g2:g3::/48`, or `host:` plus the
  last two hostname labels; `null` for a url that is not `http(s)://host[:port]`
  with an optional trailing slash, or whose host is not a canonical IPv4
  address, a bracketed IPv6 address or a lowercase ASCII hostname).
  `selectionVectors`: the witness key ids admitted, in order, from an ordered
  candidate sequence with `capacity`, `anchorCapacity` (clamped to capacity) and
  `maxPerPrefix`: anchors are considered first in the given order, then the rest
  in the given order; each admission is refused when the key id is already
  present, the anchor cap or capacity is reached, or the prefix already holds
  `maxPerPrefix` slots (anchors included). Randomizing the order of non-anchor
  candidates is the caller's job and is not part of the rule. Defaults: capacity
  5, anchorCapacity 2, maxPerPrefix 2. Many domains pointing at one machine look
  diverse because no DNS is used; the anchors are the floor.
- `shard-family-head.json` — the per-owner shard family head served by
  `GET /ledger/shard-family` (`avalon_chain::cross_shard`, `avalon_protocol::shard`).
  Generated by `crates/chain/tests/shard_family_vectors.rs` (`AVALON_REGEN_VECTORS=1`
  rewrites it). Five arrays. `familyVectors`: `input.owner` and `input.heads` (in the
  given order, each `shardId`, `treeSize`, `rootHashHex`, `signingKeyId`, `signatureHex`;
  `treeSize` is a JSON number, or a decimal string at 2^53 or more) and `expected`
  `rootHashHex`, `shardCount` and `memberShardIds` (the heads that belong to the owner,
  bytewise sorted by shard id). `proofVectors`: `input` `owner`, `rootHashHex`, `shardId`,
  `head` and `proof` (`leafIndex`, `treeSize`, `pathHex`), `expected.verified`; false for
  every failure, including a malformed root hash or a path element that is not 32 bytes.
  `familyOwnerVectors`: `shardId` to `expectedOwner` (or null), `ownerIdVectors`: `owner`
  to a boolean (a well-formed owner id has no instance), `memberVectors`: `owner` and
  `shardId` to a boolean. Whitespace in an owner is Unicode White_Space as in Rust
  `char::is_whitespace`. Supported in all three SDKs.
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
- `identity-id.json` — self-certifying identity ids
  (`avalon_protocol::identity_id`): `identity_id = lowercase_hex(SHA-256("avalon-identity-id-v1"
  || ed25519_public_key_32_bytes))`, the full 256 bits as exactly 64 characters of `[0-9a-f]`
  with no prefix. Vectors carry a `kind`: `derive` (`seedHex`, `publicKeyHex`; expected
  `preimageHex` and `identityId`), `parse` (strict: uppercase, 63 and 65 characters, `id:` and
  `node:` prefixes, UUID text and surrounding whitespace are all rejected, never normalised),
  `key_acceptability` (canonical encoding and not small-order, the same policy as the `node:`
  shard key; y = 3 is accepted) and `distinct_from_shard_id` (the identity id differs from the
  `node:` id of the same key). A public key is lowercase HEX inside signing bytes and standard
  BASE64 on the wire.
- `structured-signing-bytes.json` — the structured signing-bytes primitive
  (`avalon_protocol::signing_bytes`): domain tag, u16 BE version, then fields in a
  fixed order (u32-BE length-prefixed strings and bytes, raw keys, hashes and
  UUIDs, fixed-width big-endian integers). `vectors` build the bytes from a field
  list and must read back; `rejectVectors` must fail with exactly `expected.error`
  (`tag_mismatch`, `truncated`, `invalid_utf8`, `trailing_bytes`). Boundary cases:
  empty and 65536-byte fields, `:` `,` and NUL, multi-byte UTF-8, integer extremes.
  The file's `description` defines the field-object format. Rust only today.
- `domain-tags.json` — the registry of domain tags, one per signed kind
  (`avalon_protocol::signing_bytes::tags`); a runner asserts its registry equals
  this list. Rust only today.
- `identity-created-signing.json` — `identity.created` v2 signing bytes
  `avalon:identity.created:v2:{len(network_id)}:{network_id}:{len(shard_id)}:{shard_id}:{ticket_id}:{identity_id}:{public_key_hex}:{display_name}`
  (`len` = decimal UTF-8 BYTE length, not character count; one vector uses multi-byte ids, because network and shard ids are variable-width and shard
  ids contain `:`; ticket id = the server-issued `register/start` ticket; shard id = the shard the
  entry is issued in; display name last, `:` and multi-byte UTF-8 in it taken as-is), as UTF-8 and
  hex, with the deterministic Ed25519 signature (verified strictly). `replayVectors` list signatures
  that must NOT verify when the ticket, network or shard differs or a `:` is moved between fields.
- `device-grant-approval.json` — device grant approval bytes
  `avalon:device_grant.approved:v2:{grant_id}:{identity_id}:{requested_public_key_hex}` and
  signature.
- `signing-key-revoked.json` — signing-key revocation bytes
  `avalon:identity.signing_key_revoked:v2:{identity_id}:{signing_key_id}:{revoked_by_signing_key_id}`
  and signature; key ids are UUID text, so no `:` can occur in them. These four files are asserted
  by all three SDK runners (and by the protocol runner). `identity-id.json` also has `strict_verify`
  vectors: Ed25519 verification must be strict (S below the group order L, and no small-order R
  or key), and `identity-created-signing.json` has `domainSeparationVectors` showing the v1 and
  v2 bytes differ. The server's existing non-strict verify sites (`auth.rs`
  `verify_event_signature` and similar) accept a small-order R that `verify_strict` rejects, so
  the server switches identity-bound verification to `ed25519_key::verify_strict_signature`, and
  the SDKs reject small-order R and S >= L.

## Both sides of the wire

Unlike the four client-behavior vectors above, `attestation-signing.json`,
`signed-tree-head.json`, `witness-cosigned-tree-head.json`, `witness-announce.json`, `node-request.json`,
`self-certifying-tree-head.json`, `shard-family-head.json`, `known-list-selection.json`, `identity-chain.json`,
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
