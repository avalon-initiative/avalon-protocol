# Achievements and Attestations

**An achievement is an issuer attestation, not a `user_id → achievement_id`
row.** The durable fact is "Integrator A asserts that Identity X accomplished Y", signed
by Integrator A's key, with a timestamp and a schema. Avalon records that claim and
its provenance. **It never dictates what another integrator does with it.**

Narrative: [`../stakeholders/Proposal.md` §8](../../../stakeholders/Proposal.md#8-achievements-and-history) and
[§9](../../../stakeholders/Proposal.md#9-trust-and-attestations).

The attestation mechanism itself is domain-agnostic — any issuer, integrator or
otherwise, can assert a claim about an identity — and is illustrated below with
gaming examples because gaming is Avalon's first live use case.

**Vocabulary is category-driven, mechanism is not.** A `Issuer::Game` issuer's claims
are **Achievements**, exactly as everything below describes. A `Issuer::App`/
`Issuer::Service` issuer's claims are **Milestones** — the same record shape
(`id`/`issuer`/`name`/`description`/`schema`/`version` for a definition;
`id`/`issuer`/`subject`/`<claim>`/`issued_at`/`proof`/`revoked_at` for an
attestation), the same verification path, the same namespacing pattern
(`app:<slug>:milestone:<key>` / `service:<slug>:milestone:<key>` instead of
`game:<slug>:achievement:<key>`) — only the human-facing label and the `GlobalId`
"kind" segment vary, derived from the issuer's own category so the label can never
drift from what the issuer actually is. One shared term across both non-game
categories, not a third word for services: a service issuer's "user completed
onboarding" and an app issuer's "user hit their 100th session" are the same kind of
fact from Avalon's point of view. Cross-issuer reading (an integrator reading an app's
claims, or vice versa) works by construction, not convention — a consumer verifies a
claim without ever caring what it's called.

An issuer that wants to attach a custom shape to its own claims (a `schema`
reference) already can, for any category — see
[integrator-space.md](./integrator-space.md)'s schema-publication mechanism, itself
already category-agnostic despite its still-gaming-flavored name.

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

## Issuer trust signals

The previous section answers *meaning* — should a specific achievement be
recognized. It doesn't answer a layer below that: should an issuer be
visible/considered at all. Issuer key lifecycle already answers *authenticity* (the
signature checks out, was valid when issued); *recognition*'s missing third leg —
who decides an issuer is worth paying attention to in the first place, across
potentially many independent games/apps/services — is answered by an open,
universally-visible issuer directory, never a federated/web-of-trust model. Every
registered issuer is always visible and verifiable everywhere — nothing is ever
structurally hidden or gated behind another issuer vouching for it, the same
"mirrors, not federation" posture the settlement layer already takes: a new issuer
being invisible until someone already-inside vouches for it would recreate the
walled-garden shape this project has already rejected once.

Instead: **objective, computable reputation signals**, not a binary trust verdict from
any authority, attached to each issuer in the directory — issuance volume, issuer age,
revocation rate (key-lifecycle history already makes this a real, queryable fact), and
anything else derivable directly from the ledger itself. Each consuming
integrator/client sets its own *display* threshold over those signals; nothing is ever
invisible by default, and no issuer needs anyone's permission to exist in the
directory.

Every signal above is either already derivable from the ledger (issuance counts,
revocation rate) or fits naturally alongside what node/network status and shard
registry endpoints already expose — an issuer's signals are a read-side aggregation
over data this protocol already keeps, not a new kind of durable state or a new trust
primitive. This directory work is currently on hold.

## Definitions

An integrator defines its achievements before it issues them. An
`AchievementDefinition` carries the namespaced id, the issuer, a name, a
description, an optional `schema` reference, a `version`, and an optional
visual identity (`icon`/`icon_url`). Definitions are
durable (`achievement.defined`) so the registry and the Hub can render an
issued attestation even after the integrator is gone. `version` is bumped by
`achievement.definition_updated`; the id never changes, so a consumer can
notice a definition evolved without losing track of what it is. A definition
can be retired (`achievement.definition_retired`) — no new issuances against
it — without deleting it or touching any attestation already issued.

### Icons

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
game-event schema, not a separate mechanism — see
[`./cross-integrator-events.md`](./cross-integrator-events.md).

## Issuance is signed, not merely authenticated

Two independent proofs, doing different jobs, both required:

1. **The HTTP-level challenge-response** (`integrators::authenticate_integrator`)
   proves "this request came from whoever holds this issuer's key" — the
   same mechanism every other issuer-credentialed endpoint in this repo
   uses.
2. **A detached signature embedded in the request body** proves something
   stronger and more durable: that the issuer's key specifically vouches
   for *this exact attestation* — `attestation_signing_bytes(claim_kind,
   issuer_ref, subject, achievement)` is what gets signed, resolved
   against the issuer's full key history at the moment of issuance (a
   since-rotated-but-not-yet-revoked key still verifies correctly). This is
   the proof that would still hold up even if the HTTP layer's own auth were
   somehow bypassed — the node operator can never produce a valid
   attestation for an issuer it doesn't control.

Issuing also requires the **subject user's own consent**: an active
`IntegratorBinding` plus an active grant for `achievements.issue` (Integrator) or
`milestones.issue` (App/Service) — the same capability-check infrastructure
`presence::update_integrator_presence` already uses. Neither proof substitutes for
the other.

## Current implementation

- `crates/protocol/src/achievements.rs` — `AchievementDefinition`,
  `Issuer::Game(IntegratorId)`/`App`/`Service` (`Issuer::claim_kind()`),
  `Signature { key_id, algorithm, bytes }`, `AchievementAttestation { id,
  issuer, subject, achievement, issued_at, proof: Signature }` — **no
  `revoked_at`**: a mutable status field on durable history contradicts the
  append-only-history invariant; revocation is its own append-only entry.
  `attestation_signing_bytes(claim_kind, issuer_ref, subject, achievement)`
  is the canonical bytes an issuer's key signs, folding in `claim_kind` so a
  signature can never be replayed across vocabularies. `TrustRelationship`
  is unchanged, still unscoped.
- `crates/protocol/src/ids.rs` — `GlobalId::new(namespace, owner, kind, key)`,
  `AttestationId`, `IdentityId` (`Display`-able, `"{subject}"` in the
  signing bytes above).
- The Rust SDK's `Session::achievements()`, `issue_achievement()` and
  `issue_achievements_bulk()` check the capability, then call the server
  endpoint below with an idempotency key.
- `crates/server/src/achievements.rs` — `POST
  /integrations/{slug}/achievements/{key}/issue` /
  `POST /integrations/{slug}/milestones/{key}/issue`: verifies the caller
  is a game/app/service (never a user session), that the subject user
  has an active binding + grant for the route's issue capability, that the
  definition exists and isn't retired, and that the embedded signature
  verifies against one of the issuer's currently-valid keys — in that
  order, any failure short-circuits before the next check runs. Stores the
  attestation in `achievement_attestations` (no `revoked_at` column) and emits
  `achievement.issued`/`milestone.issued` atomically. Verified live against a
  real Postgres, including the tampered-signature, non-bound-subject, and
  retired-definition rejection paths (`crates/server/tests/attestations.rs`).
- **Per-network issuer registration gate** (`crates/server/src/issuer_registration.rs`).
  Signatures themselves stay network-agnostic (`attestation_signing_bytes` never binds
  `network_id`); network isolation is enforced by a second, independent check layered
  after the signature-authenticity check in the write path above: is this exact public
  key admitted to write on *this server's own network*? An `issuer_network_registrations`
  table is the admission list — deliberately not the same table as `issuer_keys`
  (integrator key custody/rotation), which has no network concept at all. `POST
  /issuers/registration-challenge` + `POST /issuers/register` are the explicit,
  self-service, never-reviewed registration path (a short-lived nonce, then an Ed25519
  proof-of-possession signature over `issuer_ref`+`declared_network_id`+the nonce).
  Admission policy differs by network tier, not by the handler: `avalon-dev-*`/
  `avalon-int-*` networks also auto-register a key the first time it's seen on a valid
  signed write (so calling the registration endpoint explicitly is optional there);
  `avalon-mainnet-*` (and any unrecognized `network_id`, failing closed) never does —
  an unregistered key's write is rejected outright. Verified live, including the
  auto-registration path, the explicit registration round trip, single-use-challenge
  enforcement, and both rejection paths (`crates/server/tests/issuer_registration.rs`).
- **SDK/CLI target-network declaration + mismatch check** — the SDK's
  `register_issuer` and the CLI's `register-issuer` command. Client-side
  belt-and-suspenders on top of the server-side `declared_network_id` check above: a
  caller must explicitly declare a target network (an exact `network_id`, or a
  `Dev`/`Int`/`Mainnet` tier shorthand resolved against the *verified* entry, never the
  server URL alone), and `register_issuer` refuses, entirely client-side, to send any
  request at all if that declared target doesn't match what independent STH-based
  network verification confirms the server actually is. Verified live against a real
  local dev server, including the true `Verified`-and-matching success path
  (the Rust SDK's `tests/issuer_registration.rs`, `--ignored`).
- **Bulk attestation issuance** — `POST /integrations/{slug}/achievements/bulk-issue` /
  `.../milestones/bulk-issue` (`crates/server/src/achievements.rs::bulk_issue_attestation`).
  One challenge-response plus **one** signature over
  `bulk_attestation_signing_bytes` (length-prefixed so claim order/boundaries can
  never be ambiguous) covers a whole ordered claim list for one subject; every claim
  still becomes its own ordinary `achievement_attestations` row and its own
  `achievement.issued`/`milestone.issued` event through the exact same write path
  single-claim issuance uses — no new attestation shape, no change to
  authenticity/validity/revocation. A bulk call is never all-or-nothing: an unknown or
  retired definition fails that one claim
  (`ACHIEVEMENT_DEFINITION_NOT_FOUND`/`ATTESTATION_DEFINITION_RETIRED`) without
  touching the rest of the batch. SDK: `Session::issue_achievements_bulk` —
  achievements only, no milestones SDK wrapper yet. Verified live against a real
  Postgres, including the all-valid, partial-failure, forged-signature,
  tampered-claim-list, and empty-list rejection paths, plus a regression check that
  single-claim issuance is unaffected (`crates/server/tests/achievements_bulk.rs`,
  the Rust SDK's `tests/achievements.rs`, both `--ignored`).
- **SDK revocation wrapper** — `Session::revoke_attestation`, wired to `POST
  /attestations/{id}/revoke`. Same two-proof shape as issuance (challenge-response
  plus an embedded signature over `revocation_signing_bytes`), issuer-only, no
  idempotency key needed (a repeat call against an already-revoked attestation is a
  conflict error, never a silent no-op or a second revocation). Deliberately no
  bulk-revoke counterpart to bulk-issue: a bulk-issued attestation is already an
  ordinary, independently-revocable attestation, so remediating one out of a batch is
  just `revoke_attestation` on its own id. Verified live, including the
  validity-flips-to-invalid case, already-revoked rejection, and a different-issuer
  forbidden case (the Rust SDK's `tests/achievements.rs`, `--ignored`).
- `crates/server/src/attestations.rs::list_my_achievements` — `GET
  /me/achievements?integrator_id=&claim_kind=&before=&limit=`, cursor-paginated
  newest-first (`issued_at`, `id` composite), optionally scoped to one issuer
  (`integrator_id`, the real indexed foreign key, not the `"<category>:<slug>"` wire
  string) or one claim vocabulary (`claim_kind=achievement`/`milestone`, joining
  `integrators` only when this filter is actually used). One integrator-category/status
  query, one issuer-keys query, and one revocation query are batched across the whole
  page, instead of running per attestation row. A composite `(subject, issued_at, id)`
  and `(subject, integrator_id)` index backs this. The response is `{ achievements,
  next_cursor }`, not a bare array — the SDK's `Session::achievements()` and the Hub's
  `api/achievements.ts::listMyAchievements` both request the server's max page size
  (200) and unwrap the envelope rather than exposing pagination themselves; a real
  paginated/filtered entry point on either client is a follow-up, not built here.
- `crates/server/src/achievements.rs` — claim-definition CRUD, one shared
  implementation for both vocabularies (thin per-route wrappers over a shared core):
  `POST /integrations/{slug}/achievements` / `POST /integrations/{slug}/milestones`
  (create, 409 on a duplicate key for that issuer), the matching `PATCH .../{key}`
  (update name/description/schema and bump `version`, and/or retire), and the matching
  `GET` (public listing). Write endpoints are issuer-credential-authenticated
  (challenge-response) and reject an issuer acting under another issuer's slug with
  403 — and reject an issuer whose actual registered category doesn't match the
  route's claim vocabulary (a `Game` hitting the milestones route, or an `App`/
  `Service` hitting the achievements route), including on the `GET` list route, so a
  category mismatch never silently serves one issuer's claims back mislabeled through
  the other route. The id is always `<namespace>:<slug>:<achievement|milestone>:<key>`
  and is immutable. `achievement_definitions` is one shared rebuildable projection for
  both vocabularies, written into the outbox in the same transaction as the row
  change. Covered by real live integration tests
  (`crates/server/tests/achievements.rs`, `crates/server/tests/milestones.rs`), the
  latter specifically exercising the category-enforcement behavior.
- `achievement_definitions` carries `icon`/`icon_url`, both optional. `create`/`update`
  validate `icon` against the fixed built-in set and `icon_url` against an
  `http`/`https` scheme; `AchievementDefinitionResponse.icon` always comes back
  populated (falls back to the hardcoded default, `trophy`, when the row has neither
  field set) while `icon_url` stays `Option`. `packages/ui`'s
  `AvalonAchievementCard` renders the built-in icon or an `<img>` for `icon_url` when
  present. Verified live, including the default-fallback and rejected-input paths
  (`crates/server/tests/achievements.rs`).

## Open questions

The issuer trust-signal directory described above is decided in shape but currently
on hold, not implemented.
</content>
</invoke>
