# Integrators and Issuers

**Every participating integrator has a durable issuer identity with registered
signing keys, a key history, and a status.** Attestations are only as
verifiable as the issuer record behind them. **The node operator is never the
issuer**: hosted infrastructure transports, indexes, and settles an integrator's
claims; it cannot sign them.

Narrative: [`../stakeholders/Proposal.md` §18](../../../stakeholders/Proposal.md#18-game-registration).

## Registration

An integrator connecting to Avalon establishes:

| Field | Notes |
|---|---|
| integrator id | `game:ashen-realms` — namespaced, globally unique |
| issuer identity | today the same thing as the integrator; modeled separately so other issuer kinds stay possible |
| signing key(s) | public keys, each with an id, algorithm, validity window |
| key history | every add / rotate / revoke / expire as an appended event |
| status | `ACTIVE` · `SUSPENDED` · `REVOKED` · `DEPRECATED` |
| category | `game` · `app` · `service` — additive, defaults to `game`, doesn't rename the `integrator id`/issuer vocabulary above |
| capabilities requested | what it will ask identities for — a request, never a grant |
| public metadata | name, developer, website, and similar |
| supported protocol version / Avalon features | what the integrator actually implements |
| recognition policy (optional) | which other issuers it publicly recognizes |

Registering grants nothing. An identity still authorizes each capability through
their own binding ([`./bindings.md`](./bindings.md)).

`POST /integrations` and `POST /integrations/{slug}/keys` are rate-limited
per source by default (`AVALON_INTEGRATOR_REGISTRATION_RATE_LIMIT_PER_MINUTE`,
needing no hoster configuration) — see
[`network-trust-anchors.md`](./network-trust-anchors.md)'s "Domain-proven
names" section, which also covers the separate, self-certifying-shard-id
naming path (`node:<key-hash>`) that exists alongside `game:<slug>` without
touching this registration flow.

Achievement ids are namespaced under the integrator:

```text
game:ashen-realms:achievement:dragon_slayer
game:ashen-realms:integrator_event:avalon-championship-2027:winner
```

## Issuer lifecycle

```text
Integrator registers
      ↓
Issuer identity created            issuer.registered
      ↓
Integrator declares signing key(s)       issuer.key_added
      ↓
Integrator issues attestations           achievement.issued  (signed by the integrator's key)
      ↓
Avalon records and indexes         settlement + projection
      ↓
Integrator B verifies
    ├── signature                  against the key valid at issued_at
    ├── issuer identity            exists, matches
    ├── key validity               not revoked/expired as of issued_at
    ├── attestation status         not revoked/superseded as of now
    ├── issuer status              per Integrator B's policy
    └── recognition policy         Integrator B's own decision
```

## Key management requirements

The protocol must handle all of the following without inventing cryptography:

- **rotation** — a new key is added, the old one retired; every historical
  claim signed by the old key stays verifiable
- **multiple active keys** — regions, environments, staged rotation
- **historical keys** — a retired key is still resolvable for verification of
  claims from its validity window
- **compromise** — a key is revoked as of time T; claims signed after T are
  rejected, claims signed before T are untouched
- **expiry** — keys carry validity windows
- **timestamping** — `issued_at` plus the log position pin a claim to a time
- **historical verification** — "was this key legitimate for this issuer when
  this was signed" is a first-class query

The system distinguishes "this claim was signed by the legitimate key at the
time" from "this key is compromised now". Rotating a key never invalidates
history.

A shard operator's *settlement* signing key (distinct key domain, see
"Three key domains, kept apart" below) is held to the same custody
standard: [`./self-hosting.md`](./self-hosting.md)'s managed-hosting
design never lets a managed host hold that key, for the identical
reason an issuer's key is never handed to Avalon's own infrastructure —
hosting the infrastructure around a key is never the same thing as holding
the key.

## Who authorizes key changes

**A two-tier key role**, not a single self-service key. Every issuer has
exactly one **root key**, established at registration, which is the only
key that can author `issuer.key_added`/`issuer.key_revoked` — it governs the
key *set*, never signs attestations itself in the common case. Zero or more
**operational keys**, added/revoked by the root, sign the actual
`achievement.issued`-style attestation events day to day; multiple
concurrent operational keys (regions, environments, staged rotation) are
ordinary.

Registration still requires exactly one keypair, not two — the key an integrator
registers with doubles as both its root key and its first operational key by
default, so a first-time/solo registrant pays no extra friction. A studio
that wants the isolation benefit (keep the root cold, only expose an
operational key to CI) can get there later with a single `issuer.key_added`
call, never a mandatory extra step at signup.

The reason for the split, not a single self-service key: with one key
authorized to both sign attestations *and* manage the key set, a leaked
operational key (the realistic leak scenario — it's the one embedded in a
build/CI pipeline) could also be used to revoke the legitimate developer's
keys and register the attacker's own, hijacking the issuer outright. Root
governance removes that race — the root revokes a leaked operational key the
moment it's noticed, with no window where the leaked key could also seize
key-set control. Root key loss/compromise itself is a known, honestly
unsolved residual risk, the same shape as identity recovery, not addressed
by this design.

A node operator cannot add a key to an issuer it does not control, root or
operational. Suspending or revoking an issuer at the network level is a
separate operator action with its own explicit authorization and audit
trail, not fully specified here.

## Three key domains, kept apart

| Key | Belongs to | Governs |
|---|---|---|
| identity key | the identity | mutations to the identity's own data |
| issuer key | the integrator | attestations the integrator issues |
| log operator key | the settlement operator | signing log entries / tree heads |

They may share primitives; they are never the same key and never the same
lifecycle.

Sharded settlement adds a fourth row, not a fourth mechanism: a shard
operator's settlement-signing key is authorized through this exact same
issuer-key registration flow, scoped with a `purpose: "shard_settlement"`
field alongside the implicit `"attestation"` purpose above — see
[`./network-trust-anchors.md`](./network-trust-anchors.md)'s "Per-shard
trust anchors" section for the full design. Registration, rotation, and
revocation all reuse this section's existing mechanics unchanged; only
what the key is authorized to sign differs by purpose.

## Scenarios

**E — Integrator A rotates its (operational) signing key.** Its root key authors
`issuer.key_added(k2)`, then `issuer.key_revoked(k1, reason: rotated)`. A
claim signed by k1 in 2027 verifies against k1's validity window and stays
authentic.

**F — Integrator A's operational key is compromised.** The root key authors
`issuer.key_revoked(k1, at: T)`. Claims signed by k1 before T remain
authentic and valid; claims after T are rejected. Integrator A continues issuing
under k2. The compromised key k1 was never itself capable of authorizing
this revocation (or adding a replacement) — only the root key can, per the
key-role split above — so there is no race against an attacker also holding k1.

## Current implementation

- `crates/protocol/src/integrators.rs` — `Integrator { id, slug, name, developer,
  registered_at, status, category }` (`IntegratorStatus`: `Active`/
  `Suspended`/`Revoked`/`Deprecated`; `category` is `IntegratorCategory` —
  `Game`/`App`/`Service`, defaults to `Game`), `IntegratorRegistration {
  integrator, requested_capabilities, initial_key }`, `IssuerKeyInfo {
  key_id, algorithm, public_key }` (the registration-time wire shape),
  `IntegratorCredential { integrator_id, key_id }`. `KeyRole { Root,
  Operational }` and `IssuerKey { key_id, algorithm, public_key, role,
  valid_from, valid_until, revoked_at }` — the domain type an issuer's full
  key history is made of — plus pure, I/O-free point-in-time resolvers
  `resolve_valid_signing_key`/`resolve_valid_root_key` (an attestation's
  signing key is valid if it resolves for the attestation's own
  `issued_at`; a key-set change requires the authenticating key to resolve
  as root specifically), unit-tested directly against scenarios E and F
  above.
- `crates/server/src/integrators.rs` — `POST /integrations` registers an integrator: slug
  (unique, lowercase `[a-z0-9-]`, 409 on collision — enforced with a unique
  index + `is_unique_violation()`, same pattern `guilds.rs::create_guild`
  uses), name, developer, `requested_capabilities`, and an initial Ed25519
  public key, all recorded atomically with a `game.registered` event via the
  outbox pattern. `category` (`game`/`app`/`service`) is optional in the
  request body; omitted means `game`. Projection tables: `integrators`,
  `integrator_requested_capabilities`, and `issuer_keys`
  (`key_id, integrator_id, algorithm, public_key, role, valid_until, revoked_at,
  revoked_reason, created_at`). The key recorded at registration is always
  `role: root`, doubling as the issuer's first operational key too, per the
  "zero extra friction at signup" design. `game.registered`'s
  `issuer`/`subject` name the integrator itself but the event is
  network-attributed, not integrator-signed (the registrant's key isn't
  proven to control anything yet at that point). `POST
  /integrations/{slug}/challenge` + `authenticate_integrator`/`GET
  /integrations/whoami` implement a challenge-response authentication scheme:
  a short-lived random nonce (`integrator_challenges`, same ephemeral-ceremony
  shape as `webauthn_ceremonies`), signed by the integrator's registered key,
  verified via `auth::verify_event_signature` — `authenticate_integrator`
  filters out revoked/expired keys. Registering grants no capability — the
  connection/consent flow (`crates/server/src/connections.rs`) owns the
  actual grant/consent logic.
- **Key-set management**: `POST /integrations/{slug}/keys` (add) and `POST
  /integrations/{slug}/keys/{key_id}/revoke`, both requiring
  `authenticate_integrator_root` — the same challenge-response scheme as
  `authenticate_integrator`, but additionally requiring the authenticating key to
  resolve as a currently-valid **root** key for the named issuer
  (`AppError::IssuerKeyNotRoot` if an otherwise-valid operational key
  tries). Emit `issuer.key_added`/`issuer.key_revoked`. Revoking an
  already-revoked or nonexistent key is a 403
  (`AppError::IssuerKeyForbidden`), not a silent no-op. Verified live against
  a real Postgres: adding an operational key with root auth, confirming that
  key authenticates ordinary calls but is rejected (401) for key management,
  root revoking it, the revoked key then failing to authenticate *anything*,
  and a repeat revoke correctly rejected rather than succeeding twice.
- **`GET /integrations/{slug}/keys`**: the read side of the above — public,
  unauthenticated, returns every key this issuer has ever registered (any
  role, any status), oldest first, giving the Hub's integrator-profile page a
  way to show real key history and status. Verified live: root key plus one
  added-then-revoked operational key, fetched with no auth headers at all,
  both entries present with the revoked one's `revoked_at` set.
- **Still deferred**: the network-level authorization model for transitioning
  `IntegratorStatus` into `Suspended`/`Revoked`/`Deprecated` (the enum
  variants exist; nothing can set them yet), and root-key loss/compromise
  recovery (an honestly-flagged residual risk).
- **`/integrations` is the canonical public API path.** Every integrator route
  lives under `/integrations` (including `/integrations/{slug}/challenge` and
  `/integrations/whoami`), and the `x-avalon-integrator-*` header spelling is
  the only one accepted.
- `crates/protocol/src/achievements.rs` — `Issuer::Game(IntegratorId)`, plus
  additive sibling variants `Issuer::App(IntegratorId)` and
  `Issuer::Service(IntegratorId)`, mirroring `IntegratorCategory` — each
  mints its own `app:`/`service:` `GlobalId` namespace prefix via
  `Issuer::namespace()`, parallel to (never replacing) `Issuer::Game`'s
  `game:` namespace.
- `crates/cli` — `avalon register-integrator --slug <slug> --name <name>
  --owner-name <owner> [--capability <cap>]... [--server <url>]`: generates a
  fresh Ed25519 keypair locally, calls `POST /integrations` with the public
  key, saves the private key to `_running/keys/integrator-<slug>.signing-key`
  (mirroring `create-identity`'s local-key persistence) and prints it once
  with a loss-of-key warning — the server only ever stores the public half.
  Also exercises the challenge-response round trip (`POST
  /integrations/{slug}/challenge` → `GET /integrations/whoami`, sending the
  `x-avalon-integrator-*` header names) once as a sanity check. A slug
  collision (409) prints a clear message instead of a raw HTTP error.
</content>
</invoke>
