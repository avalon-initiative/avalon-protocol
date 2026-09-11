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
| category | `game` · `app` · `service` (#282, decision #275) — additive, defaults to `game`, doesn't rename `game id`/issuer vocabulary above |
| capabilities requested | what it will ask identities for — a request, never a grant |
| public metadata | name, developer, website, and similar |
| supported protocol version / Avalon features | what the game actually implements |
| recognition policy (optional) | which other issuers it publicly recognizes |

Registering grants nothing. An identity still authorizes each capability through
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

**Decided ([#80](https://github.com/LunarVagabond/avalon-protocol/issues/80)):
a two-tier key role**, not a single self-service key. Every issuer has
exactly one **root key**, established at registration, which is the only
key that can author `issuer.key_added`/`issuer.key_revoked` — it governs the
key *set*, never signs attestations itself in the common case. Zero or more
**operational keys**, added/revoked by the root, sign the actual
`achievement.issued`-style attestation events day to day; multiple
concurrent operational keys (regions, environments, staged rotation) are
ordinary.

Registration still requires exactly one keypair, not two — the key a game
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
unsolved residual risk — the same shape as
[#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) (identity
recovery), not addressed by this decision.

A node operator cannot add a key to an issuer it does not control, root or
operational. Suspending or revoking an issuer at the network level is a
separate operator action with its own explicit authorization and audit
trail — that authorization model is left to #84's implementation, not fully
specified by #80's decision.

## Three key domains, kept apart

| Key | Belongs to | Governs | Tracked in |
|---|---|---|---|
| identity key | the identity | mutations to the identity's own data | #73 |
| issuer key | the game | attestations the game issues | #80 / #84 |
| log operator key | the settlement operator | signing log entries / tree heads | #39 |

They may share primitives; they are never the same key and never the same
lifecycle.

## Scenarios

**E — Game A rotates its (operational) signing key.** Its root key authors
`issuer.key_added(k2)`, then `issuer.key_revoked(k1, reason: rotated)`. A
claim signed by k1 in 2027 verifies against k1's validity window and stays
authentic.

**F — Game A's operational key is compromised.** The root key authors
`issuer.key_revoked(k1, at: T)`. Claims signed by k1 before T remain
authentic and valid; claims after T are rejected. Game A continues issuing
under k2. The compromised key k1 was never itself capable of authorizing
this revocation (or adding a replacement) — only the root key can, per #80's
decided key-role split above — so there is no race against an attacker also
holding k1.

## Today in the repo

- `crates/protocol/src/games.rs` — `Game { id, slug, name, developer,
  registered_at, status, category }` (`GameStatus`, now `Active`/
  `Suspended`/`Revoked`/`Deprecated` — see #84 note below; `category` is
  `IntegratorCategory` — `Game`/`App`/`Service`, #282, defaults to `Game`),
  `GameRegistration { game, requested_capabilities, initial_key }`,
  `IssuerKeyInfo { key_id, algorithm, public_key }` (the registration-time
  wire shape), `GameCredential { game_id, key_id }`. **#84 (implementing
  #80's decided two-tier key model)**: `KeyRole { Root, Operational }` and
  `IssuerKey { key_id, algorithm, public_key, role, valid_from,
  valid_until, revoked_at }` — the domain type an issuer's full key history
  is made of — plus pure, I/O-free point-in-time resolvers
  `resolve_valid_signing_key`/`resolve_valid_root_key` (an attestation's
  signing key is valid if it resolves for the attestation's own
  `issued_at`; a key-set change requires the authenticating key to resolve
  as root specifically), unit-tested directly against scenarios E and F
  above.
- `crates/server/src/games.rs` (#26) — `POST /games` registers a game: slug
  (unique, lowercase `[a-z0-9-]`, 409 on collision — enforced with a unique
  index + `is_unique_violation()`, same pattern `guilds.rs::create_guild`
  uses), name, developer, `requested_capabilities`, and an initial Ed25519
  public key, all recorded atomically with a `game.registered` event via the
  outbox pattern. `category` (`game`/`app`/`service`, #282) is optional in
  the request body; omitted means `game`. Projection tables: `games`
  (migration `0040_game_category` added the `category` column, `NOT NULL
  DEFAULT 'game'`), `game_requested_capabilities`, and `issuer_keys`
  (`key_id, game_id, algorithm, public_key, role, valid_until, revoked_at,
  revoked_reason, created_at` — migration `0044_issuer_key_lifecycle`
  extended the original registration-only shape per #84). The key recorded
  at registration is always `role: root`, doubling as the issuer's first
  operational key too, per #80's decided "zero extra friction at signup."
  `game.registered`'s `issuer`/`subject` name the game itself but the event
  is network-attributed, not game-signed — see the module doc comment on
  why (the registrant's key isn't proven to control anything yet at that
  point). `POST /games/{slug}/challenge` + `authenticate_game`/`GET
  /games/whoami` implement the milestone-1 challenge-response stand-in
  this ticket's own text calls for pending #80 (now decided; this scheme's
  own future is separate from that decision): a short-lived random nonce
  (`game_challenges`, same ephemeral-ceremony shape as
  `webauthn_ceremonies`), signed by the game's registered key, verified via
  `auth::verify_event_signature` — `authenticate_game` now also filters out
  revoked/expired keys (#84), where it previously trusted any key ever
  registered indefinitely. Registering grants no capability — #27 owns the
  actual grant/consent logic.
- **Key-set management (#84)**: `POST /games/{slug}/keys` (add) and `POST
  /games/{slug}/keys/{key_id}/revoke`, both requiring
  `authenticate_game_root` — the same challenge-response scheme as
  `authenticate_game`, but additionally requiring the authenticating key to
  resolve as a currently-valid **root** key for the named issuer
  (`AppError::IssuerKeyNotRoot` if an otherwise-valid operational key
  tries). Emit `issuer.key_added`/`issuer.key_revoked`. Revoking an
  already-revoked or nonexistent key is a 403
  (`AppError::IssuerKeyForbidden`), not a silent no-op — matching this
  ticket's "surface a caller retrying against a key it no longer controls"
  posture. Verified live against a real Postgres: adding an operational key
  with root auth, confirming that key authenticates ordinary calls but is
  rejected (401) for key management, root revoking it, the revoked key then
  failing to authenticate *anything*, and a repeat revoke correctly
  rejected rather than succeeding twice.
- **Still deferred (#84's own explicit scope note)**: the network-level
  authorization model for transitioning `GameStatus` into `Suspended`/
  `Revoked`/`Deprecated` (the enum variants exist; nothing can set them
  yet), and root-key loss/compromise recovery (#80's own honestly-flagged
  residual risk, the same shape as #99).
- **`/integrations` as the canonical public API path (#293)**: `POST
  /integrations` dual-routes to the same registration handler as `POST
  /games` (not a redirect — a `POST` redirect silently becomes a `GET` in
  many clients); `GET /integrations`/`GET /integrations/{slug}` are
  canonical, with `GET /games`/`GET /games/{slug}` kept working as real HTTP
  redirects. The challenge-response auth headers gained generic
  `x-avalon-integrator-key-id`/`x-avalon-integrator-challenge-id`/
  `x-avalon-integrator-signature` equivalents alongside the original
  `x-avalon-game-*` names — either is accepted from a caller; this repo's
  own SDK/CLI send only the new name. Neither the `/games/{slug}/challenge`
  and `/games/whoami` paths nor any JSON field were renamed by this pass.
- `crates/protocol/src/achievements.rs` — `Issuer::Game(GameId)`; no status,
  no key set. #297 added additive sibling variants `Issuer::App(GameId)` and
  `Issuer::Service(GameId)`, mirroring `IntegratorCategory` (#282) — each
  mints its own `app:`/`service:` `GlobalId` namespace prefix via
  `Issuer::namespace()`, parallel to (never replacing) `Issuer::Game`'s
  `game:` namespace.
- `crates/cli` — `avalon register-game --slug <slug> --name <name>
  --developer <dev> [--capability <cap>]... [--server <url>]` (#29): generates
  a fresh Ed25519 keypair locally, calls `POST /integrations` (#293's
  canonical alias for `POST /games`) with the public key, saves the private
  key to `_running/keys/game-<slug>.signing-key` (mirroring
  `create-identity`'s local-key persistence) and prints it once with a
  loss-of-key warning — the server only ever stores the public half. Also
  exercises the challenge-response round trip
  (`POST /games/{slug}/challenge` → `GET /games/whoami`, sending the
  `x-avalon-integrator-*` header names) once as a sanity check. A slug
  collision (409) prints a clear message instead of a raw HTTP error.
  `avalon register-integrator` (#297) is now an additive alias for the same
  command — identical parsing and behavior, `register-game` still works too.

## Decisions and tickets

- [#25](https://github.com/LunarVagabond/avalon-protocol/issues/25) — Epic:
  Game Registration & Permissions.
- [#26](https://github.com/LunarVagabond/avalon-protocol/issues/26) —
  registration endpoint + credential issuance.
- [#29](https://github.com/LunarVagabond/avalon-protocol/issues/29) —
  `avalon register-game`.
- [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) —
  decision, closed: issuer signing keys and lifecycle — two-tier root/
  operational key roles, described above. Root key loss/compromise
  recovery is an explicitly open residual risk, not resolved by this
  decision.
- [#275](https://github.com/LunarVagabond/avalon-protocol/issues/275) —
  decision: additive `category` field (game/app/service), no rename of
  `GameId`/`Issuer::Game`/the `games` table/`game.*` event kinds.
- [#282](https://github.com/LunarVagabond/avalon-protocol/issues/282) —
  implementation of #275, described above.
- [#293](https://github.com/LunarVagabond/avalon-protocol/issues/293) —
  `/integrations` as the canonical public API path, `/games` kept as a
  compatibility path, generic `x-avalon-integrator-*` auth headers,
  described above.
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity implementation, now unblocked by #80's decision above.
- [#297](https://github.com/LunarVagabond/avalon-protocol/issues/297) —
  additive `Issuer::App`/`Issuer::Service` variants and the
  `register-integrator` CLI alias, described above.
- [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) — ledger
  entry signing; the operator's key, a separate domain.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) — identity
  keys; a separate domain.
