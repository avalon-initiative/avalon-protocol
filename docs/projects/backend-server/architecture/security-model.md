# Security Model

**Authority is scoped. No single actor — not an integrator, not a node operator, not
the network itself — is omnipotent.** Users control their identity, integrators
control their own worlds and their own attestations, guilds govern themselves,
and infrastructure transports, indexes, settles, and verifies without owning
any of it. **Hosted infrastructure is not protocol authority.**

## Who controls what

| Actor | Controls | Cannot |
|---|---|---|
| **User** | identity keys; profile declarations; social actions; permission grants and visibility; guild participation | issue attestations about themselves; rewrite issued history |
| **Integrator** | its integrator profiles/bindings; its integrator-side characters and progression; attestations under its own issuer key; its recognition policy | touch another integrator's profile or characters; issue under another issuer's identity; alter an identity's history or unrelated guild history |
| **Guild** | governance, membership, roles, settings, channels | act as an issuer; reach into a member's other data |
| **Avalon infrastructure** | transport, indexing, settlement, discovery, verification | fabricate an issuer claim; fabricate an identity; silently become the owner of user or integrator data |

Details per actor: [`./identity.md`](./identity.md),
[`./bindings.md`](./bindings.md), [`./guilds.md`](./guilds.md),
[`./nodes.md`](./nodes.md).

## Authorization: one capability, one check

Every endpoint an integrator calls on a user's behalf must check the specific
capability it needs — never a blanket "does this integrator have access to this
user" boolean. Two caller kinds exist: a **user**, acting on their own
data (always allowed — this is specifically about *integrator* access to *user*
data, not a user's access to themselves), and an **integrator acting for a
user**, which needs both an active [binding](./bindings.md)
and an active `PermissionGrant` (see [`./privacy.md`](./privacy.md))
for exactly the capability the endpoint names.

`crates/server/src/authz.rs`'s `Caller` (`Caller::User(identity_id)` /
`Caller::Integrator { integrator_id, identity_id }`) and `require_capability(caller,
capability, state)` are this check, built as the one place it lives —
not literal Axum middleware, a plain async fn called explicitly per handler,
matching how `guilds.rs`'s `has_guild_permission` is already called rather
than injected as a layer. `require_capability`'s `capability` argument is
mandatory, not optional or defaultable, so a call site can never accidentally
check nothing. A revoked grant is rejected on the very next request — no
grace window, since every check reads `permission_grants` fresh (no cache
yet). Authorization failure for `Caller::Integrator` is `AppError::Forbidden`
(403), body identical regardless of *why* — no binding, a binding for the
wrong integrator, no grant, or a revoked grant all read the same to the
caller, matching this file's "never leak internal detail" posture below.

`Caller::Integrator` resolves by `(identity_id, integrator_id)` — read from
a new `x-avalon-identity-id` header alongside the existing
`x-avalon-integrator-*` auth headers — rather than by a `binding_id` a
caller could simply name: naming a row directly would let the guard trust
"this id must be legitimate, since it parses" instead of confirming *whose*
row it is and *which* integrator it belongs to. Resolving by the pair means
there is no `bindings` row to find at all when an integrator tries to use
one user's binding to a different integrator — a lookup miss, not a
same-row check that fails after the fact.

No in-process cache: with only a handful of real callers so far there is
nothing to profile a cache against, and a wrong invalidation rule — the one
hard part of any cache — would be actively dangerous for an authorization
check, since "revoked is rejected on the very next request" is the
invariant a stale entry would silently violate. Revisit once real call
volume exists to measure against.

## Node authority

```text
Integrator A
    signs:  Dragon Slayer → User X   (issuer key)

Hosted Avalon node
    transports, indexes, settles that claim
    cannot replace Integrator A's signature
    cannot produce "Integrator A issued …" when Integrator A did not
```

This holds because every durable claim is signed by the party with authority
over it, and the log is independently verifiable
([`./settlement.md`](./settlement.md)). The
same guarantee extends to identities: an
identity signs its own `identity.created` with its Ed25519 event-signing key,
verified independently of the WebAuthn ceremony that authenticated the
request — a node cannot mint an identity that never actually registered.

## Three key domains

| Key | Held by | Compromise means | Response |
|---|---|---|---|
| identity passkey | the identity | attacker can log in as that identity | revoke via a second registered passkey (`POST /me/devices/:id/revoke`) — total loss if it was the only one, recoverable via guardian-based recovery |
| identity event-signing key | the identity | attacker can author events for that identity going forward | rotate from an authenticated session (not built); historical events signed by the old key stay valid, same principle as issuer keys below |
| issuer key | the integrator | attacker can issue authentic-looking claims under that integrator | revoke key as of T; claims after T rejected, before T untouched |
| log operator key (Signed Tree Heads only, not per-entry) | settlement operator | attacker can sign bogus tree heads | mirrors/witnesses detect divergence via gossiped signed tree heads — no validator set |

Keys are never shared across domains. They may share primitives (established
signature schemes, existing Rust crates), never a bespoke construction.

## Key compromise, concretely

Compromise is handled by *time-bounded revocation*, not by deleting history:
a `key_revoked` entry names the key and the time T from which it is no longer
trusted. Verification of any claim resolves the key set as of that claim's
`issued_at`. "The legitimate key at the time" and "a compromised key now" are
different facts and both stay answerable —
[`./issuers.md`](./issuers.md).

## Credentials never enter the ledger

A password hash, a session token, a private key, or any other secret is never
part of a protocol event. Public keys are fine — they are public. There is no
password anywhere in the system: the `identity.created` event
carries no `username`, and there is no `credentials` table.

## Transport

`avalon-server` runs plain HTTP by default in local development. That is
acceptable only on loopback. Before any non-local deployment, TLS terminates
in front of it — a deployment requirement, not an optional hardening step.

## Explicit limitations

- **Authenticity is not meaning.** No mechanism stops an issuer from signing a
  meaningless claim ([`./trust-model.md`](./trust-model.md)).
- **A single log operator can still censor or delay appends.** Mirrors and
  verifiability make divergence detectable after the fact, but nothing forces
  liveness from one operator today. Deliberately not solved by a validator
  set — there is no contested resource for validators to referee, so a
  multi-validator chain would add real operational complexity for a
  guarantee (censorship-resistance, not tamper-evidence) it doesn't actually
  buy here. If Avalon ever runs more than one independent settlement
  operator, the real mitigation is witnessed, gossiped signed tree heads
  catching a divergent/dishonest operator — not consensus. Sharded
  settlement narrows this from "the whole network's liveness" to "one
  shard's liveness" but does not eliminate it at the single-shard level —
  see the next point for the managed-hosting case specifically.
- **A managed settlement host can still stall or refuse its own integrator,
  even though it can never forge that integrator's history.** Two-phase
  signing keeps the integrator's key off the host, so a dishonest or
  unavailable host is limited to withholding service, never fabricating
  events — but withholding service is still a real liveness gap for that
  one integrator's shard, same shape as the single-operator case above, one
  level down. Recourse: a shard's identity/trust anchor is tied to the
  integrator's own key, never to the hosting operator — switching to a
  different managed host, or to self-hosting, carries zero continuity
  break, since the new host or self-hosted node can sync the shard's
  existing log from any mirror before resuming service. An integrator is
  never cryptographically locked into one host; it can always be
  operationally slow to actually switch. See
  [`./self-hosting.md`](./self-hosting.md)'s "Managed hosting" section for
  the mechanics.
- **Statistics can be gamed.** Sybil identities can inflate registry numbers;
  documented, not solved ([`./registry.md`](./registry.md)).
- **Persistent identity makes harassment persistent.** Blocking and
  cross-integrator moderation are open questions (Proposal §31–32) and interact with
  [`./privacy.md`](./privacy.md).
- **Recovery is real but not exhaustive.** A lost passkey with no second one
  registered is total, permanent loss of the identity unless guardian-based
  M-of-N recovery was configured in advance — see [identity.md](./identity.md)
  for the current mechanics.
- **Topology endpoints are unauthenticated outbound-request triggers.**
  `POST /nodes/probe` and `POST /nodes/trace` make this node send requests to
  URLs that came from gossip. Mitigations: the target must already be in the peer table; every
  resolved address must pass the outbound address policy (link-local, cloud
  metadata, unspecified, multicast always refused; loopback and private ranges
  refused unless `AVALON_ALLOW_PRIVATE_PEERS`); the connection is pinned to
  the checked address; no redirects; at most 3 sequential requests per call;
  a per-request timeout; a dedicated per-IP rate limit and a cap on
  concurrent probes; timings only in the response. A hoster can turn the
  whole group off with `AVALON_TOPOLOGY_PUBLIC=false`. A trace forwards only
  to the active neighbor the routing rule chose, one forward per hop, with a
  hop cap of 16, a visited-list loop check, a carried time budget, clamped
  request fields, a size- and shape-checked downstream response, and its own
  per-IP limit and in-flight cap. The measurements and hop entries are
  self-reported by the nodes involved, not verified facts. See
  [`./nodes.md`](./nodes.md).
- **The peer table is fed by unauthenticated announces and gossip.** Without
  bounds, fabricated addresses could grow it without limit, spread through
  gossip, and become targets of outbound requests. Mitigations: a size cap
  that evicts the oldest inactive entry and never an active or bootstrap peer
  (`AVALON_NODE_MAX_KNOWN_PEERS`); every announced and gossiped base URL runs
  the outbound address policy and a length limit before admission; a
  per-source budget of new distinct URLs per minute and a per-exchange cap on
  gossip; a bounded `/nodes/status` reachability check with the same
  `network_id` before a new announcer is admitted; rejections are explicit
  4xx/429 responses. Residual risk: an attacker with many reachable public
  addresses and many source addresses can still churn the inactive part of the
  table. See [`./nodes.md`](./nodes.md).

## Current implementation

- `crates/server/src/auth.rs` — builds the `Webauthn` instance, verifies
  Ed25519 event signatures, generates opaque CSPRNG session tokens (revocable
  by row deletion). No passwords anywhere.
- `crates/server/src/error.rs` — internal error text never reaches the
  client.
- `crates/server/src/handlers.rs` — bearer-token auth for `me`/`update_profile`;
  unknown/expired tokens are indistinguishable to the caller. Registration
  verifies a WebAuthn ceremony and an Ed25519 event signature, both, before
  writing anything.
- `crates/chain/src/postgres.rs` — hash-chained entries, content re-verified
  on read; signed at the tree-head level, not per-entry.
- User passkeys and event-signing keys exist. Issuer keys exist with a
  two-tier root/operational model. TLS and full visibility scopes remain
  partial — see [`./privacy.md`](./privacy.md).
- `crates/server/src/authz.rs` — `Caller` / `require_capability`, built and
  exhaustively unit-tested (pure-logic matrix plus a live-Postgres matrix
  gated `--ignored`). Real callers include `presence.rs`'s
  `update_integrator_presence`, gating `presence.publish`, and
  `integrator_schemas.rs` — every other integrator-calling-the-API endpoint
  should reuse this rather than hand-rolling a check.
</content>
</invoke>
