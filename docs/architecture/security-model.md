# Security Model

**Authority is scoped. No single actor — not a game, not a node operator, not
the network itself — is omnipotent.** Players control their identity, games
control their own worlds and their own attestations, guilds govern themselves,
and infrastructure transports, indexes, settles, and verifies without owning
any of it. **Hosted infrastructure is not protocol authority.**

## Who controls what

| Actor | Controls | Cannot |
|---|---|---|
| **Player** | identity keys; profile declarations; social actions; permission grants and visibility; guild participation | issue attestations about themselves; rewrite issued history |
| **Game** | its game profiles/bindings; its game-side characters and progression; attestations under its own issuer key; its recognition policy | touch another game's profile or characters; issue under another issuer's identity; alter an identity's history or unrelated guild history |
| **Guild** | governance, membership, roles, settings, channels | act as an issuer; reach into a member's other data |
| **Avalon infrastructure** | transport, indexing, settlement, discovery, verification | fabricate an issuer claim; fabricate an identity; silently become the owner of user or game data |

Details per actor: [`./identity.md`](./identity.md),
[`./game-bindings.md`](./game-bindings.md), [`./guilds.md`](./guilds.md),
[`./nodes.md`](./nodes.md).

## Node authority

```text
Game A
    signs:  Dragon Slayer → Player X       (issuer key)

Hosted Avalon node
    transports, indexes, settles that claim
    cannot replace Game A's signature
    cannot produce "Game A issued …" when Game A did not
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
| player passkey (#73) | the player | attacker can log in as that identity | revoke/replace via a second registered passkey (#99, not built) — total loss if it was the only one |
| player event-signing key (#73) | the player | attacker can author events for that identity going forward | rotate from an authenticated session (not built); historical events signed by the old key stay valid, same principle as issuer keys below |
| issuer key (#80) | the game | attacker can issue authentic-looking claims under that game | revoke key as of T; claims after T rejected, before T untouched |
| log operator key (#39) | settlement operator / validator | attacker can sign bogus log entries / tree heads | mirrors detect divergence; the validator set (#40) limits any one signer's rewrite power |

Keys are never shared across domains. The design for each is a separate open
decision; they may share primitives (established signature schemes, existing
Rust crates), never a bespoke construction.

## Key compromise, concretely

Compromise is handled by *time-bounded revocation*, not by deleting history:
a `key_revoked` entry names the key and the time T from which it is no longer
trusted. Verification of any claim resolves the key set as of that claim's
`issued_at`. "The legitimate key at the time" and "a compromised key now" are
different facts and both stay answerable —
[`./games-and-issuers.md`](./games-and-issuers.md).

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
  verifiability make this detectable; operator independence beyond that is why
  Avalon runs its own multi-validator chain rather than a single-operator log
  ([#79](https://github.com/LunarVagabond/avalon-protocol/issues/79), closed;
  [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93)) — the
  validator set itself is still being designed ([#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)).
- **Statistics can be gamed.** Sybil identities can inflate registry numbers;
  documented, not solved ([`./game-registry.md`](./game-registry.md)).
- **Persistent identity makes harassment persistent.** Blocking and
  cross-game moderation are open questions (Proposal §31–32) and interact with
  [`./privacy.md`](./privacy.md).
- **Recovery is unsolved.** #73 is the only login mechanism and a lost
  passkey (with no second one registered) is total, permanent loss of the
  identity today — tracked as its own open decision,
  [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99).

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
  on read; no signatures yet (#39).
- Player passkeys and event-signing keys exist (#73). No issuer keys yet
  (#80/#84), no TLS (#72), no visibility scopes (#87).

## Decisions and tickets

- [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) — ADR:
  settlement is a public transparency log.
- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) — TLS
  before any non-local deployment.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) — player
  identity as a self-custodied keypair. Done.
- [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) — open
  decision: identity recovery when every passkey is lost.
- [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) — log
  signing scheme.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) — issuer
  keys and lifecycle.
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity implementation.
- [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79) /
  [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93) —
  long-term settlement backend, decided.
