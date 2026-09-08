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
same guarantee extends to identities once
[#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) lands: an
identity signs its own creation and profile events, so a node cannot mint
identities either. Today the `identity.created` event is issued by the server,
which is the milestone-1 gap #73's comment records.

## Three key domains

| Key | Held by | Compromise means | Response |
|---|---|---|---|
| player key (#73) | the player | attacker can mutate that one identity's data | revoke/rotate via recovery path; history stays |
| issuer key (#80) | the game | attacker can issue authentic-looking claims under that game | revoke key as of T; claims after T rejected, before T untouched |
| log operator key (#39) | settlement operator | attacker can sign bogus log entries / tree heads | mirrors detect divergence; anchoring (#79) limits rewrite |

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
part of a protocol event. Public keys are fine — they are public. The
`identity.created` event carrying `username` today is a milestone-1 leftover
removed with #73/#86. Event schemas get a negative test for secret-shaped
fields ([#82](https://github.com/LunarVagabond/avalon-protocol/issues/82)).

## Transport

`avalon-server` runs plain HTTP today. That is acceptable only on loopback.
Before any non-local deployment, TLS terminates in front of it
([#72](https://github.com/LunarVagabond/avalon-protocol/issues/72)) — a
deployment blocker, not an optional hardening step.

## Explicit limitations

- **Authenticity is not meaning.** No mechanism stops an issuer from signing a
  meaningless claim ([`./trust-model.md`](./trust-model.md)).
- **A single log operator can still censor or delay appends.** Mirrors and
  verifiability make this detectable; operator independence beyond that is the
  open backend decision ([#79](https://github.com/LunarVagabond/avalon-protocol/issues/79)).
- **Statistics can be gamed.** Sybil identities can inflate registry numbers;
  documented, not solved ([`./game-registry.md`](./game-registry.md)).
- **Persistent identity makes harassment persistent.** Blocking and
  cross-game moderation are open questions (Proposal §31–32) and interact with
  [`./privacy.md`](./privacy.md).
- **Recovery is unsolved.** A lost passkey needs a designed recovery path
  before #73 is the only login (Proposal §32).

## Today in the repo

- `crates/server/src/auth.rs` — Argon2id password hashing, opaque CSPRNG
  session tokens (revocable by row deletion).
- `crates/server/src/error.rs` — internal error text never reaches the
  client.
- `crates/server/src/handlers.rs` — bearer-token auth; unknown/expired tokens
  are indistinguishable to the caller.
- `crates/chain/src/postgres.rs` — hash-chained entries, content re-verified
  on read; no signatures yet (#39).
- No issuer keys, no player keys, no TLS, no visibility scopes (#87).

## Decisions and tickets

- [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) — ADR:
  settlement is a public transparency log.
- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) — TLS
  before any non-local deployment.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) — player
  identity as a self-custodied keypair.
- [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) — log
  signing scheme.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) — issuer
  keys and lifecycle.
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity implementation.
- [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79) —
  long-term settlement backend.
