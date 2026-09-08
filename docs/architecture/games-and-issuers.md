# Games and Issuers

**Every participating game has a durable issuer identity with registered
signing keys, a key history, and a status.** Attestations are only as
verifiable as the issuer record behind them. **The node operator is never the
issuer**: hosted infrastructure transports, indexes, and settles a game's
claims; it cannot sign them.

Narrative: [`../stakeholders/Proposal.md` §18](../stakeholders/Proposal.md#18-game-registration).

## Registration

A game connecting to Avalon establishes:

| Field | Notes |
|---|---|
| game id | `game:ashen-realms` — namespaced, globally unique |
| issuer identity | today the same thing as the game; modeled separately so other issuer kinds stay possible |
| signing key(s) | public keys, each with an id, algorithm, validity window |
| key history | every add / rotate / revoke / expire as an appended event |
| status | `ACTIVE` · `SUSPENDED` · `REVOKED` · `DEPRECATED` |
| capabilities requested | what it will ask players for — a request, never a grant |
| public metadata | name, developer, website, and similar |
| supported protocol version / Avalon features | what the game actually implements |
| recognition policy (optional) | which other issuers it publicly recognizes |

Registering grants nothing. A player still authorizes each capability through
their own binding ([`./game-bindings.md`](./game-bindings.md)).

Achievement ids are namespaced under the game:

```text
game:ashen-realms:achievement:dragon_slayer
game:ashen-realms:game_event:avalon-championship-2027:winner
```

## Issuer lifecycle

```text
Game registers
      ↓
Issuer identity created            issuer.registered
      ↓
Game declares signing key(s)       issuer.key_added
      ↓
Game issues attestations           achievement.issued  (signed by the game's key)
      ↓
Avalon records and indexes         settlement + projection
      ↓
Game B verifies
    ├── signature                  against the key valid at issued_at
    ├── issuer identity            exists, matches
    ├── key validity               not revoked/expired as of issued_at
    ├── attestation status         not revoked/superseded as of now
    ├── issuer status              per Game B's policy
    └── recognition policy         Game B's own decision
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

## Who authorizes key changes

Adding, rotating, or revoking an issuer key is authorized by the issuer's own
existing key (or a root key, depending on
[#80](https://github.com/LunarVagabond/avalon-protocol/issues/80)). A node
operator cannot add a key to an issuer it does not control. Suspending or
revoking an issuer at the network level is a separate operator action with its
own explicit authorization and audit trail.

## Three key domains, kept apart

| Key | Belongs to | Governs | Tracked in |
|---|---|---|---|
| player key | the identity | mutations to the identity's own data | #73 |
| issuer key | the game | attestations the game issues | #80 / #84 |
| log operator key | the settlement operator | signing log entries / tree heads | #39 |

They may share primitives; they are never the same key and never the same
lifecycle.

## Scenarios

**E — Game A rotates its signing key.** `issuer.key_added(k2)`, then
`issuer.key_revoked(k1, reason: rotated)`. A claim signed by k1 in 2027 verifies
against k1's validity window and stays authentic.

**F — Game A's key is compromised.** `issuer.key_revoked(k1, at: T)`. Claims
signed by k1 before T remain authentic and valid; claims after T are rejected.
Game A continues issuing under k2.

## Today in the repo

- `crates/protocol/src/games.rs` — `Game { id, slug, name, developer,
  registered_at }`, `GameRegistration { game, requested_capabilities }`,
  `GameCredential { game_id, key_id }`. The credential is a key *id* with no
  public key behind it, so nothing can be verified.
- `crates/protocol/src/achievements.rs` — `Issuer::Game(GameId)`; no status,
  no key set.
- `crates/server` — no registration endpoint yet (#26).
- `crates/cli` — no `register-game` yet (#29, #48).

## Decisions and tickets

- [#25](https://github.com/LunarVagabond/avalon-protocol/issues/25) — Epic:
  Game Registration & Permissions.
- [#26](https://github.com/LunarVagabond/avalon-protocol/issues/26) —
  registration endpoint + credential issuance.
- [#29](https://github.com/LunarVagabond/avalon-protocol/issues/29) —
  `avalon register-game`.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) —
  decision, open: issuer signing keys and lifecycle.
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity implementation (blocked by #80).
- [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) — ledger
  entry signing; the operator's key, a separate domain.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) — player
  keys; a separate domain.
