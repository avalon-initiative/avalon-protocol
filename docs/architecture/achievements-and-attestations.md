# Achievements and Attestations

**An achievement is an issuer attestation, not a `user_id → achievement_id`
row.** The durable fact is "Integrator A asserts that Identity X accomplished Y", signed
by Integrator A's key, with a timestamp and a schema. Avalon records that claim and
its provenance. **It never dictates what another integrator does with it.**

Narrative: [`../stakeholders/Proposal.md` §8](../stakeholders/Proposal.md#8-achievements-and-history) and
[§9](../stakeholders/Proposal.md#9-trust-and-attestations).

The attestation mechanism itself is domain-agnostic — any issuer, integrator or
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
Cross-issuer reading (an integrator reading an app's claims, or vice versa) works
by construction, not convention — a consumer verifies a claim without ever
caring what it's called.

An issuer that wants to attach a custom shape to its own claims (a
`schema` reference) already can, for any category — see
[integrator-space.md](./integrator-space.md)'s schema-publication mechanism (#181/#255),
itself already category-agnostic despite its still-gaming-flavored name
(tracked as a pending rename under #290, not re-decided here).

## Shape

```text
Issuer (Integrator A, signing key k1)
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

The signature answers "did Integrator A issue this". Nothing in the attestation
answers "was this hard", and nothing can — see
[`./trust-model.md`](./trust-model.md).

## Namespacing

Human-readable names are never globally unique. Achievement ids are namespaced
under the issuing integrator via `GlobalId`:

```text
game:ashen-realms:achievement:dragon_slayer
game:worldzero:achievement:dragon_slayer
game:random-mmo-47:achievement:dragon_slayer
```

Three distinct claims that happen to share a title. The display name stays
"Dragon Slayer"; provenance makes the distinction.

## Same title, different provenance

- Integrator A issues Dragon Slayer after a brutal endgame raid.
- Integrator B issues Dragon Slayer after a different hard achievement.
- Integrator C lets every user click a button labelled Dragon Slayer.

All three are cryptographically authentic. Avalon does not pretend they are
semantically identical, and it does not rank them. The Hub shows each with its
issuer; a consuming integrator recognizes whichever it chooses; an identity's owner
features or hides whichever they like.

## The receiving integrator decides meaning

Integrator A: "User X defeated the Dragon Lord."
Integrator B may unlock a title. Integrator C may unlock a quest. Integrator D may ignore it.

A consuming integrator verifies authenticity and validity (universal) and then
applies its own recognition policy (contextual). The SDK exposes those three
results separately so an integrator can *display* claims it doesn't *recognize*.

## Definitions

An integrator defines its achievements before it issues them. An
`AchievementDefinition` carries the namespaced id, the issuer, a name, a
description, an optional `schema` reference, a `version`, and an optional
visual identity (`icon`/`icon_url`, #332). Definitions are
durable (`achievement.defined`) so the registry and the Hub can render an
issued attestation even after the integrator is gone. `version` is bumped by
`achievement.definition_updated`; the id never changes, so a consumer can
notice a definition evolved without losing track of what it is. A definition
can be retired (`achievement.definition_retired`) — no new issuances against
it — without deleting it or touching any attestation already issued.

### Icons (#332)

A definition gets a visual identity without every integrator needing to host
anything: `icon` is a key into a small, fixed built-in icon set shipped with
`packages/ui` (`trophy`/`star`/`shield`/`sword` — generic enough to cover
games/apps/services alike), and `icon_url` is an integrator-hosted image that
takes precedence over `icon` when present. Both are optional; a definition
with neither set still renders the hardcoded default (`trophy`) rather than a
blank slot — the server, not the client, owns that fallback, so every reader
(the Hub, a future third-party consumer) sees the same default without
reimplementing the choice. `icon_url` is stored as-is and validated only for
an `http`/`https` scheme, never fetched or re-hosted — the same
"the server records, it doesn't vouch for content" posture `avatar_url`
already has in this schema.

## Lifecycle

```text
achievement.defined  →  achievement.issued  →  (achievement.revoked | attestation.superseded)
```

Every step is an appended protocol event. Revocation never removes the issuance
— see [`./revocation.md`](./revocation.md). Integrator event results (tournaments,
seasonal championships, community campaigns, ...) are attestations with a
integrator-event schema, not a separate mechanism — see
[`./cross-integrator-events.md`](./cross-integrator-events.md).

## Issuance is signed, not merely authenticated (#32)

Two independent proofs, doing different jobs, both required:

1. **The HTTP-level challenge-response** (`integrators::authenticate_integrator`, #26)
   proves "this request came from whoever holds this issuer's key" — the
   same mechanism every other issuer-credentialed endpoint in this repo
   uses.
2. **A detached signature embedded in the request body** proves something
   stronger and more durable: that the issuer's key specifically vouches
   for *this exact attestation* — `attestation_signing_bytes(claim_kind,
   issuer_ref, subject, achievement)` is what gets signed, resolved
   against the issuer's full key history at the moment of issuance via
   #84's `resolve_valid_signing_key` (a since-rotated-but-not-yet-revoked
   key still verifies correctly). This is the proof that would still hold
   up even if the HTTP layer's own auth were somehow bypassed — matching
   #32's own invariant that the node operator can never produce a valid
   attestation for an issuer it doesn't control.

Issuing also requires the **subject user's own consent**: an active
`IntegratorBinding` plus an active grant for `achievements.issue` (Integrator) or
`milestones.issue` (App/Service) — #28's `Caller`/`require_capability`
guard, the same infrastructure `presence::update_integrator_presence` already
uses. Neither proof substitutes for the other.

## Today in the repo

- `crates/protocol/src/achievements.rs` — `AchievementDefinition`,
  `Issuer::Game(IntegratorId)`/`App`/`Service` (`Issuer::claim_kind()` — #324),
  `Signature { key_id, algorithm, bytes }`, `AchievementAttestation { id,
  issuer, subject, achievement, issued_at, proof: Signature }` — **no
  `revoked_at`** (#32, per #81's decided revocation mechanics: a mutable
  status field on durable history is exactly what #75's ADR forbids;
  revocation is its own append-only entry, tracked as #85, not built yet),
  `attestation_signing_bytes(claim_kind, issuer_ref, subject, achievement)`
  — the canonical bytes an issuer's key signs, folding in `claim_kind` so a
  signature can never be replayed across vocabularies. `TrustRelationship`
  unchanged, still unscoped (#33).
- `crates/protocol/src/ids.rs` — `GlobalId::new(namespace, owner, kind, key)`,
  `AttestationId`, `IdentityId` (now `Display`-able, `"{subject}"` in the
  signing bytes above).
- `crates/sdk/src/lib.rs` — `Session::achievements()` and
  `issue_achievement()` check the capability, then return `NotImplemented`
  — still true; #32 landed the server endpoint, not the SDK client (#34).
- `crates/server/src/achievements.rs` (#32) — `POST
  /integrations/{slug}/achievements/{key}/issue` /
  `POST /integrations/{slug}/milestones/{key}/issue`: verifies the caller
  is a game/app/service (never a user session), that the subject user
  has an active binding + grant for the route's issue capability, that the
  definition exists and isn't retired, and that the embedded signature
  verifies against one of the issuer's currently-valid keys — in that
  order, any failure short-circuits before the next check runs. Stores the
  attestation in `achievement_attestations` (migration
  `0045_achievement_attestations`, no `revoked_at` column) and emits
  `achievement.issued`/`milestone.issued` atomically (#71's pattern).
  Verified live against a real Postgres, including the tampered-signature,
  non-bound-subject, and retired-definition rejection paths
  (`crates/server/tests/attestations.rs`).
- `crates/server/src/achievements.rs` (#31, generalized to App/Service by
  #324/#325) — claim-definition CRUD, one shared implementation for both
  vocabularies (thin per-route wrappers over a shared core — see the
  module's own doc comment): `POST /integrations/{slug}/achievements` /
  `POST /integrations/{slug}/milestones` (create, 409 on a duplicate key
  for that issuer), the matching `PATCH .../{key}` (update
  name/description/schema and bump `version`, and/or retire), and the
  matching `GET` (public listing). Write endpoints are
  issuer-credential-authenticated (`integrators::authenticate_integrator`, #26's
  challenge-response scheme) and reject an issuer acting under another
  issuer's slug with 403 — and, new for #324/#325, reject an issuer whose
  actual registered category doesn't match the route's claim vocabulary
  (a `Game` hitting the milestones route, or an `App`/`Service` hitting
  the achievements route), including on the `GET` list route, so a
  category mismatch never silently serves one issuer's claims back
  mislabeled through the other route. The id is always
  `<namespace>:<slug>:<achievement|milestone>:<key>` and is immutable.
  `achievement_definitions` is one shared rebuildable projection for both
  vocabularies, written into the outbox in the same transaction as the row
  change (#71's pattern). Covered by real live integration tests
  (`crates/server/tests/achievements.rs`, `crates/server/tests/milestones.rs`),
  the latter specifically exercising the category-enforcement behavior.
  Issuing (signed attestations) is #32, described in its own section above.
- `achievement_definitions` (migration `0047_achievement_definition_icons`,
  #332) — `icon`/`icon_url`, both optional. `create`/`update` validate
  `icon` against the fixed built-in set and `icon_url` against an
  `http`/`https` scheme (`achievements::validate_icon`/`validate_icon_url`,
  reusing `handlers::is_http_url`); `AchievementDefinitionResponse.icon`
  always comes back populated (falls back to the hardcoded default,
  `trophy`, when the row has neither field set) while `icon_url` stays
  `Option`. `packages/ui`'s `AvalonAchievementCard` (#35) renders the
  built-in icon or an `<img>` for `icon_url` when present. Verified live,
  including the default-fallback and rejected-input paths
  (`crates/server/tests/achievements.rs`).

## Decisions and tickets

- [#324](https://github.com/LunarVagabond/avalon-protocol/issues/324) —
  decided: category-driven claim vocabulary (Achievement for
  `Issuer::Game`, Milestone for `Issuer::App`/`Issuer::Service`), same
  mechanism throughout — described above.
- [#325](https://github.com/LunarVagabond/avalon-protocol/issues/325) —
  implements #324: the Milestone CRUD slice for `Issuer::App`/
  `Issuer::Service`, described above (`crates/server/src/achievements.rs`).
- [#30](https://github.com/LunarVagabond/avalon-protocol/issues/30) — Epic:
  Achievements & Attestations.
- [#31](https://github.com/LunarVagabond/avalon-protocol/issues/31) —
  definition CRUD per integrator. Its App/Service (Milestone) equivalent is
  #325, landed.
- [#32](https://github.com/LunarVagabond/avalon-protocol/issues/32) — issue
  an achievement/milestone → signed attestation, landed for both claim
  vocabularies (#324). SDK client (#34) still returns `NotImplemented`.
- [#33](https://github.com/LunarVagabond/avalon-protocol/issues/33) — verify
  attestation + trust relationships.
- [#34](https://github.com/LunarVagabond/avalon-protocol/issues/34) — SDK
  methods wired to a real server.
- [#35](https://github.com/LunarVagabond/avalon-protocol/issues/35) — Hub
  achievements list.
- [#332](https://github.com/LunarVagabond/avalon-protocol/issues/332) —
  default icon set + dev-hosted `icon_url` override, landed, described
  above.
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — event
  catalogue (the `achievement.*` kinds).
- [#88](https://github.com/LunarVagabond/avalon-protocol/issues/88) — integrator
  event result attestations.
