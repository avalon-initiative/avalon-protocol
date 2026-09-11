# Achievements and Attestations

**An achievement is an issuer attestation, not a `player_id → achievement_id`
row.** The durable fact is "Game A asserts that Identity X accomplished Y", signed
by Game A's key, with a timestamp and a schema. Avalon records that claim and
its provenance. **It never dictates what another game does with it.**

Narrative: [`../stakeholders/Proposal.md` §8](../stakeholders/Proposal.md#8-achievements-and-history) and
[§9](../stakeholders/Proposal.md#9-trust-and-attestations).

The attestation mechanism itself is domain-agnostic — any issuer, game or
otherwise, can assert a claim about an identity — and is illustrated below with
gaming examples because gaming is Avalon's first live use case.

**Vocabulary is category-driven, mechanism is not** (decided
[#324](https://github.com/LunarVagabond/avalon-protocol/issues/324), amending
[#290](https://github.com/LunarVagabond/avalon-protocol/issues/290)'s
original "keep exactly as-is" ruling on this specific primitive). A
`Issuer::Game` issuer's claims are **Achievements**, exactly as everything
below describes — unchanged, permanent. A `Issuer::App`/`Issuer::Service`
issuer's claims are **Milestones** — the same record shape
(`id`/`issuer`/`name`/`description`/`schema`/`version` for a definition;
`id`/`issuer`/`subject`/`<claim>`/`issued_at`/`proof`/`revoked_at` for an
attestation), the same verification path, the same namespacing pattern
(`app:<slug>:milestone:<key>` / `service:<slug>:milestone:<key>` instead of
`game:<slug>:achievement:<key>`) — only the human-facing label and the
`GlobalId` "kind" segment vary, derived from the issuer's own category so
the label can never drift from what the issuer actually is. One shared term
across both non-game categories, not a third word for services: a service
issuer's "user completed onboarding" and an app issuer's "user hit their
100th session" are the same kind of fact from Avalon's point of view.
Cross-issuer reading (a game reading an app's claims, or vice versa) works
by construction, not convention — a consumer verifies a claim without ever
caring what it's called.

An issuer that wants to attach a custom shape to its own claims (a
`schema` reference) already can, for any category — see
[game-space.md](./game-space.md)'s schema-publication mechanism (#181/#255),
itself already category-agnostic despite its still-gaming-flavored name
(tracked as a pending rename under #290, not re-decided here).

## Shape

```text
Issuer (Game A, signing key k1)
    │
    ▼
Attestation
    ├── achievement     game:ashen-realms:achievement:dragon_slayer
    ├── subject         Avalon Identity X
    ├── issued_at       2027-03-14T21:07:00Z
    ├── schema/version  achievement.v1
    ├── evidence        opaque reference the issuer chooses to attach
    ├── signature       over all of the above, by k1
    └── status          derived: Active | Revoked | Superseded
```

The signature answers "did Game A issue this". Nothing in the attestation
answers "was this hard", and nothing can — see
[`./trust-model.md`](./trust-model.md).

## Namespacing

Human-readable names are never globally unique. Achievement ids are namespaced
under the issuing game via `GlobalId`:

```text
game:ashen-realms:achievement:dragon_slayer
game:worldzero:achievement:dragon_slayer
game:random-mmo-47:achievement:dragon_slayer
```

Three distinct claims that happen to share a title. The display name stays
"Dragon Slayer"; provenance makes the distinction.

## Same title, different provenance

- Game A issues Dragon Slayer after a brutal endgame raid.
- Game B issues Dragon Slayer after a different hard achievement.
- Game C lets every player click a button labelled Dragon Slayer.

All three are cryptographically authentic. Avalon does not pretend they are
semantically identical, and it does not rank them. The Hub shows each with its
issuer; a consuming game recognizes whichever it chooses; an identity's owner
features or hides whichever they like.

## The receiving game decides meaning

Game A: "Player X defeated the Dragon Lord."
Game B may unlock a title. Game C may unlock a quest. Game D may ignore it.

A consuming game verifies authenticity and validity (universal) and then
applies its own recognition policy (contextual). The SDK exposes those three
results separately so a game can *display* claims it doesn't *recognize*.

## Definitions

A game defines its achievements before it issues them. An
`AchievementDefinition` carries the namespaced id, the issuer, a name, a
description, an optional `schema` reference, and a `version`. Definitions are
durable (`achievement.defined`) so the registry and the Hub can render an
issued attestation even after the game is gone. `version` is bumped by
`achievement.definition_updated`; the id never changes, so a consumer can
notice a definition evolved without losing track of what it is. A definition
can be retired (`achievement.definition_retired`) — no new issuances against
it — without deleting it or touching any attestation already issued.

## Lifecycle

```text
achievement.defined  →  achievement.issued  →  (achievement.revoked | attestation.superseded)
```

Every step is an appended protocol event. Revocation never removes the issuance
— see [`./revocation.md`](./revocation.md). Game event results (tournaments,
seasonal championships, community campaigns, ...) are attestations with a
game-event schema, not a separate mechanism — see
[`./game-events.md`](./game-events.md).

## Today in the repo

- `crates/protocol/src/achievements.rs` — `AchievementDefinition`,
  `Issuer::Game(GameId)`, `AchievementAttestation { id, issuer, subject,
  achievement, issued_at, proof, revoked_at }`, `TrustRelationship`. The
  `proof` bytes are opaque; no verification exists yet. `revoked_at` is a
  mutable field on the record, which #85 replaces with revocation entries.
- `crates/protocol/src/ids.rs` — `GlobalId::new(namespace, owner, kind, key)`
  and `AttestationId`.
- `crates/sdk/src/lib.rs` — `Session::achievements()` and
  `issue_achievement()` check the capability, then return `NotImplemented`.
- `crates/server/src/achievements.rs` (#31) — `AchievementDefinition` CRUD:
  `POST /games/{slug}/achievements` (create, 409 on a duplicate key for that
  game), `PATCH /games/{slug}/achievements/{key}` (update name/description/
  schema and bump `version`, and/or retire), `GET /games/{slug}/achievements`
  (public listing). The write endpoints are game-credential-authenticated
  (`games::authenticate_game`, #26's challenge-response scheme) and reject a
  game acting under another game's slug with 403; the id is always
  `game:<slug>:achievement:<key>` and is immutable. `achievement_definitions`
  is a rebuildable projection, written into the outbox in the same
  transaction as the row change (#71's pattern). No issuing yet (#32), no
  signing.

## Decisions and tickets

- [#324](https://github.com/LunarVagabond/avalon-protocol/issues/324) —
  decided: category-driven claim vocabulary (Achievement for
  `Issuer::Game`, Milestone for `Issuer::App`/`Issuer::Service`), same
  mechanism throughout — described above. #31/#32's still-open
  implementation should build this from day one, not retrofit it.
- [#30](https://github.com/LunarVagabond/avalon-protocol/issues/30) — Epic:
  Achievements & Attestations.
- [#31](https://github.com/LunarVagabond/avalon-protocol/issues/31) —
  definition CRUD per game. Still needs its App/Service (Milestone)
  equivalent per #324.
- [#32](https://github.com/LunarVagabond/avalon-protocol/issues/32) — issue an
  achievement → signed attestation.
- [#33](https://github.com/LunarVagabond/avalon-protocol/issues/33) — verify
  attestation + trust relationships.
- [#34](https://github.com/LunarVagabond/avalon-protocol/issues/34) — SDK
  methods wired to a real server.
- [#35](https://github.com/LunarVagabond/avalon-protocol/issues/35) — Hub
  achievements list.
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — event
  catalogue (the `achievement.*` kinds).
- [#88](https://github.com/LunarVagabond/avalon-protocol/issues/88) — game
  event result attestations.
