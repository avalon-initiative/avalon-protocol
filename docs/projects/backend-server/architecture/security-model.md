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
data, not a user's access to themselves), and a **integrator acting for a
user**, which needs both an active [binding](./bindings.md)
(`#83`) and an active `PermissionGrant` (see [`./privacy.md`](./privacy.md))
for exactly the capability the endpoint names.

`crates/server/src/authz.rs`'s `Caller` (`Caller::User(identity_id)` /
`Caller::Integrator { integrator_id, identity_id }`) and `require_capability(caller,
capability, state)` (#28) are this check, built as the one place it lives —
not literal Axum middleware, a plain async fn called explicitly per handler,
matching how `guilds.rs`'s `has_guild_permission` is already called rather
than injected as a layer. `require_capability`'s `capability` argument is
mandatory, not optional or defaultable, so a call site can never accidentally
check nothing. A revoked grant is rejected on the very next request — no
grace window, since every check reads `permission_grants` fresh (no cache
yet; see the module's own doc comment for why). Authorization failure for
`Caller::Integrator` is `AppError::Forbidden` (403), body identical regardless of
*why* — no binding, a binding for the wrong integrator, no grant, or a revoked
grant all read the same to the caller, matching this file's "never leak
internal detail" posture below.

**First real caller: `presence::update_integrator_presence`** (#16's
`PUT /presence/:identity_id`), gated on an active `presence.publish` grant.
Every other endpoint is still either user-session-only (`friends.rs`,
`guilds.rs`, `connections.rs` — a grant is a user action, an integrator
never grants itself anything) or proves only the integrator's own identity
with nothing user-specific to check (`integrators::integrator_whoami`).
`Caller`/`require_capability` remain the infrastructure any future
integrator-calling-the-API endpoint (achievement issuance, etc.) should
reuse rather than hand-rolling its own check.

`Caller::Integrator` resolves by `(identity_id, integrator_id)` — read from
a new `x-avalon-identity-id` header alongside the existing
`x-avalon-integrator-*` auth headers — rather than by a `binding_id` a
caller could simply name: naming a row directly would let the guard trust
"this id must be legitimate, since it parses" instead of confirming *whose*
row it is and *which* integrator it belongs to. Resolving by the pair means
there is no `bindings` row to find at all when an integrator tries to use
one user's binding to a different integrator — a lookup miss, not a
same-row check that fails after the fact.

No in-process cache: with only one real caller so far there is nothing to
profile a cache against, and a wrong invalidation rule — the one hard part
of any cache — would be actively dangerous for an authorization check,
since "revoked is rejected on the very next request" is the invariant a
stale entry would silently violate. Revisit once real call volume exists to
measure against.

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
([`./settlement.md`](./settlement.md),
[ADR #70](https://github.com/LunarVagabond/avalon-protocol/issues/70)). The
same guarantee now extends to identities
([#73](https://github.com/LunarVagabond/avalon-protocol/issues/73), done): an
identity signs its own `identity.created` with its Ed25519 event-signing key,
verified independently of the WebAuthn ceremony that authenticated the
request — a node cannot mint an identity that never actually registered.
`profile.updated` doesn't emit an event at all yet ([#86](https://github.com/LunarVagabond/avalon-protocol/issues/86)),
so that gap remains until #86 lands.

## Three key domains

| Key | Held by | Compromise means | Response |
|---|---|---|---|
| identity passkey (#73) | the identity | attacker can log in as that identity | revoke via a second registered passkey (`POST /me/devices/:id/revoke`, #135) — total loss if it was the only one, recoverable via guardian-based recovery (#99, decided; #201) |
| identity event-signing key (#73) | the identity | attacker can author events for that identity going forward | rotate from an authenticated session (not built); historical events signed by the old key stay valid, same principle as issuer keys below |
| issuer key (#80) | the integrator | attacker can issue authentic-looking claims under that integrator | revoke key as of T; claims after T rejected, before T untouched |
| log operator key (#39, decided: Signed Tree Heads only, not per-entry) | settlement operator | attacker can sign bogus tree heads | mirrors/witnesses detect divergence via gossiped signed tree heads — no validator set (#186); implementation tracked by #210 |

Keys are never shared across domains. The design for each is a separate open
decision; they may share primitives (established signature schemes, existing
Rust crates), never a bespoke construction.

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
password anywhere in the system anymore (#73): the `identity.created` event
no longer carries a `username`, and there is no `credentials` table. Event
schemas get a negative test for secret-shaped fields
([#82](https://github.com/LunarVagabond/avalon-protocol/issues/82)).

## Transport

`avalon-server` runs plain HTTP today. That is acceptable only on loopback.
Before any non-local deployment, TLS terminates in front of it
([#72](https://github.com/LunarVagabond/avalon-protocol/issues/72)) — a
deployment blocker, not an optional hardening step.

## Explicit limitations

- **Authenticity is not meaning.** No mechanism stops an issuer from signing a
  meaningless claim ([`./trust-model.md`](./trust-model.md)).
- **A single log operator can still censor or delay appends.** Mirrors and
  verifiability make divergence detectable after the fact, but nothing forces
  liveness from one operator today. Deliberately not solved by a validator
  set — there is no contested resource for validators to referee, so a
  multi-validator chain would add real operational complexity for a
  guarantee (censorship-resistance, not tamper-evidence) it doesn't actually
  buy here ([ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186)).
  If Avalon ever runs more than one independent settlement operator, the
  real mitigation is witnessed, gossiped signed tree heads catching a
  divergent/dishonest operator — not consensus. Sharded settlement (#527)
  narrows this from "the whole network's liveness" to "one shard's
  liveness" but does not eliminate it at the single-shard level — see the
  next point for the managed-hosting case specifically.
- **A managed settlement host (#531) can still stall or refuse its own
  integrator, even though it can never forge that integrator's history.**
  #531's two-phase signing keeps the integrator's key off the host, so a
  dishonest or unavailable host is limited to withholding service, never
  fabricating events — but withholding service is still a real liveness
  gap for that one integrator's shard, same shape as the single-operator
  case above, one level down. Recourse (#544): a shard's identity/trust
  anchor is tied to the integrator's own key, never to the hosting
  operator (#543) — switching to a different managed host, or to
  self-hosting, carries zero continuity break, since the new host or
  self-hosted node can sync the shard's existing log from any mirror
  (#529/#530) before resuming service. An integrator is never
  cryptographically locked into one host; it can always be operationally
  slow to actually switch. See
  [`./self-hosting.md`](./self-hosting.md)'s "Managed hosting" section for
  the mechanics.
- **Statistics can be gamed.** Sybil identities can inflate registry numbers;
  documented, not solved ([`./registry.md`](./registry.md)).
- **Persistent identity makes harassment persistent.** Blocking and
  cross-integrator moderation are open questions (Proposal §31–32) and interact with
  [`./privacy.md`](./privacy.md).
- **Recovery is decided, not yet fully landed.** A lost passkey with no
  second one registered was total, permanent loss of the identity;
  [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) (decided)
  settled a guardian-based M-of-N recovery design, implemented by
  [#201](https://github.com/LunarVagabond/avalon-protocol/issues/201) — see
  [identity.md](./identity.md) for the current mechanics.

## Today in the repo

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
  on read; signed at the tree-head level, not per-entry (#39, decided;
  #210, implementation).
- User passkeys and event-signing keys exist (#73). No issuer keys yet
  (#80/#84), no TLS (#72), no visibility scopes (#87).
- `crates/server/src/authz.rs` (#28) — `Caller` / `require_capability`, built
  and exhaustively unit-tested (pure-logic matrix plus a live-Postgres
  matrix gated `--ignored`). No longer unused: `presence.rs`'s
  `update_integrator_presence` calls it to gate `presence.publish` (#16), and
  `integrator_schemas.rs` reuses it too — every other integrator-calling-the-API
  endpoint is still hypothetical and should reuse this rather than
  hand-rolling a check.

## Decisions and tickets

- [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) — ADR:
  settlement is a public transparency log.
- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) — TLS
  before any non-local deployment.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) —
  identity as a self-custodied keypair. Done.
- [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) —
  decided: identity recovery when every passkey is lost (guardian-based
  M-of-N), implementation #201.
- [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) —
  decided: log signing scheme (Signed Tree Heads only), implementation
  #210.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) — issuer
  keys and lifecycle.
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity implementation.
- [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79) /
  [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93) —
  long-term settlement backend, decided.
- [#28](https://github.com/LunarVagabond/avalon-protocol/issues/28) —
  permission enforcement (`Caller` / `require_capability`). Built; no caller
  yet. Any future endpoint letting an integrator act on a user's behalf must use
  this guard rather than its own check.
