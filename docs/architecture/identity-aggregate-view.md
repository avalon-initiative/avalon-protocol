# An Identity's Aggregate View: Avalon-Native Data vs. Integrator Block Space

[`./worked-ledger-example.md`](./worked-ledger-example.md) renders one
user's ledger as an ordered *event log* — the sequence of things that
happened, over time, in the actual `ProtocolEvent` envelope. This document
is different: it renders the same identity as a single **aggregate
snapshot** — what a full picture of "this identity, right now" looks like
once every relevant event has been folded together. Neither document is the
ledger itself; the ledger is the ordered event sequence
(`worked-ledger-example.md`'s subject). This one is a read-side projection
of it, the shape an indexer, an SDK, or a public API would hand back.

No single existing endpoint returns exactly the shape below today — it is
assembled conceptually from several real, separate reads (`GET /me`, the
guild-membership list, the friends list, per-subject attestations, integrator
bindings). Treat the shape as the intended target for a future aggregate
read, not a literal endpoint response — every field in it is a real,
cited field from an existing type; the *assembly* is illustrative.

## The two layers

**Layer 1 — Avalon-native, portable identity data.** Everything under
direct control of the identity's own owner (with two narrow exceptions:
guild role changes go through guild governance, per
[`./guilds.md`](./guilds.md); friendship requires both parties' consent,
per [`./social-graph.md`](./social-graph.md)). This is the data that
follows the identity across every integrator, app, and service it ever touches —
identity, profile self-description, friends, guild memberships. See
[`./identity.md`](./identity.md)'s "Self-described metadata is
self-expression, not fact."

**Layer 2 — integrator block space.** Everything scoped to one specific
game, app, or service — a *binding* (the identity's opt-in connection to
that integrator, [`./bindings.md`](./bindings.md)) plus whatever
attestations that integrator has issued about the identity under its own
issuer key ([`./achievements-and-attestations.md`](./achievements-and-attestations.md)).
Game, App, and Service are one unified concept —
`avalon_protocol::integrators::IntegratorCategory` (issue #282, decided #275) —
not three separate systems; a "layer 2" entry is always shaped the same
way regardless of which category it belongs to, distinguished only by its
`type`.

## The shape

```json
{
  "identity": {
    "id": "a1b2c3d4-...-000001",
    "handle": "LV#4821",
    "created_at": "2027-01-04T09:12:03Z"
  },
  "profile": {
    "display_name": "LV",
    "avatar_url": "https://...",
    "banner_url": "https://...",
    "bio": "Full-time dragon slayer.",
    "status": "Raiding tonight",
    "pronouns": "she/her",
    "favorite_genres": ["rpg", "mmo"],
    "links": ["https://..."],
    "timezone": "America/Los_Angeles",
    "theme_color": "#7c3aed",
    "location": "Pacific Northwest",
    "main_guild": "g-1",
    "effective_main_guild": "g-1"
  },
  "guilds": [
    {"guild_id": "g-1", "role": "officer", "joined_at": "2027-02-01T00:00:00Z"}
  ],
  "friends": ["a1b2c3d4-...-000002", "a1b2c3d4-...-000003"],
  "integrations": [
    {
      "type": "game",
      "integrator_id": "ashen-realms",
      "binding": {"established_at": "2027-01-04T09:20:00Z", "ended_at": null},
      "attestations": [
        {
          "achievement": "game:ashen-realms:achievement:dragon-slayer",
          "issued_at": "2027-01-10T00:00:00Z",
          "issuer_key_id": "ashen-realms-op-1"
        }
      ]
    },
    {
      "type": "app",
      "integrator_id": "some-companion-app",
      "binding": {"established_at": "2027-04-01T00:00:00Z", "ended_at": null},
      "attestations": []
    },
    {
      "type": "service",
      "integrator_id": "some-tooling-service",
      "binding": {"established_at": "2027-05-01T00:00:00Z", "ended_at": null},
      "attestations": []
    }
  ]
}
```

`banner_url`/`status`/`links`/`timezone`/`theme_color`/`location` landed via
issue #372. `main_guild` and `effective_main_guild` landed with no ticket.
`main_guild` is `Profile`'s own stored field (`null` unless explicitly set).
`effective_main_guild` isn't a `Profile` field at all — it's computed at read
time, falling back to the earliest-joined guild membership when `main_guild`
is unset, and it only ever appears in a response shape, never in storage or
in a `profile.updated` payload. See
[`./identity.md`](./identity.md#what-is-promised-durable). If this doc and
the actual code ever disagree, the code is right and this doc is stale —
same discipline `worked-ledger-example.md` holds itself to.

## Field reference

The field-by-field table — every field's real Rust type and what it's for,
for both layers — lives in its own file so it doesn't crowd out the rest of
this document: [`./identity-aggregate-view-fields.md`](./identity-aggregate-view-fields.md).

## A made-up integrator's full shape, illustrated

The `integrations` example above is deliberately minimal. Here is one entry
fleshed all the way out for a hypothetical integrator, **"Emberfall Online"**, to
make the two genuinely different mechanisms layer 2 contains impossible to
confuse: canonical attestations (real, built, uniform across every
integrator) versus an integrator's own custom-shaped data (partially real
— schema publication exists; actual instance data does not, yet).

```json
{
  "type": "game",
  "integrator_id": "emberfall-online",
  "binding": {
    "established_at": "2027-06-01T14:00:00Z",
    "ended_at": null
  },
  "attestations": [
    {
      "achievement": "game:emberfall-online:achievement:first-blood",
      "issued_at": "2027-06-01T14:32:00Z",
      "issuer_key_id": "emberfall-op-1"
    },
    {
      "achievement": "game:emberfall-online:achievement:dungeon-master",
      "issued_at": "2027-07-15T09:10:00Z",
      "issuer_key_id": "emberfall-op-1"
    }
  ],
  "published_schemas": [
    {
      "id": "game:emberfall-online:schema:character:v1",
      "version": 1,
      "proto_source": "message Character { string name = 1; uint32 level = 2; string race = 3; string class = 4; repeated string titles = 5; }",
      "default_visibility": "private",
      "field_visibility": {"name": "public", "level": "public", "class": "public"}
    }
  ],
  "characters": {
    "_status": "BUILT — #384, decided #381; GET /identities/{id}/integrator-data",
    "instances": [
      {
        "schema": "game:emberfall-online:schema:character:v1",
        "integrator_id": "11111111-1111-1111-1111-111111111111",
        "published_at": "2027-06-01T14:35:00Z",
        "fields": {
          "name": "Vesryn",
          "level": 42,
          "class": "Ranger"
        }
      }
    ]
  }
}
```

Note what's missing from `fields` above: `race` and `titles` were part of the
`Character` instance Emberfall Online actually published (`name`, `level`,
`race`, `class`, `titles` — matching the schema's five fields), but the
schema's `default_visibility: "private"` plus a `field_visibility` override
naming only `name`/`level`/`class` as `"public"` means the read endpoint
drops `race` and `titles` from the response entirely — not null, not
present-but-redacted, just absent, per the bidirectional visibility rule in
`crate::integrator_data::resolve_visible_fields`.

Three distinct pieces, three different rules:

- **`attestations`** — real, built, and the only one of the three that is
  *canonical across every integrator*. Emberfall Online cannot invent its
  own shape for these; every entry is a `GlobalId` +
  `AchievementAttestation`, identical in structure to any other integrator's,
  app's, or service's attestations. This is what makes a generic "show me
  this user's achievements from anywhere" view possible at all.
- **`published_schemas`** — real, built (`game_schema.published`, issue
  #255; see
  [`./worked-ledger-example.md`](./worked-ledger-example.md#what-an-integrators-own-custom-fact-looks-like-in-the-same-ledger)),
  now also carrying `default_visibility`/`field_visibility` (#381/#384).
  Emberfall Online *can* define its own arbitrary `Character` shape here —
  this is genuinely integrator-custom, by design, because nothing outside
  Emberfall Online is expected to know what a "Character" means for this
  specific integrator — but that shape is now actually parsed and validated
  (real protobuf, not stored opaquely), not just accepted as an opaque
  string, so a malformed schema is rejected cleanly rather than silently
  stored.
- **`characters`** — **decided (#381) and built (#384).** This is what
  `GET /identities/{id}/integrator-data` actually returns: every current
  (non-superseded) instance published about Nova, across every
  integrator/schema, each already filtered to only the fields that instance's
  schema currently makes visible — the caller never sees the full raw
  instance and never has to apply the visibility rule itself. Deliberately
  narrow by design, not by limitation: the intent is small, portable,
  *fun-to-carry-across-integrators* flavor data — name, level, race, class,
  titles — never a character's full mechanical state (inventory, skills,
  stats used for game balance). That heavier, genuinely game-critical data
  has no reason to ever leave an integrator's own database; publishing it here
  would be a design mistake even once this mechanism exists, not just
  noise. Its read-access model is decided and built: see "Read access is
  not one uniform rule" below, which now states the real policy rather
  than flagging an open question.

## Why layer 2 is safe to let anyone write into, and why it can't leak into layer 1

Every attestation an integrator issues is signed by **that integrator's
own key**, verified before it's ever stored
(`avalon_chain::attestations::verify_authenticity`, issue #33) — not by
Avalon vouching for it. Issuer keys follow a decided two-tier model (issue
#80, implemented #84): every integrator has exactly one **root key**
(established at registration, the only key that can add/revoke other
keys) and one or more **operational keys** it actually signs with day to
day — a compromised operational key is revocable by the root without
touching the integrator's identity itself. See
[`./issuers.md`](./issuers.md) for the full key
lifecycle.

This document's `integrations` array depends on a clean write boundary
between integrators, and between an integrator and layer 1. See
[`./security-model.md`](./security-model.md#who-controls-what)'s "Who
controls what" table for the full statement of that boundary.

Concretely: **Avalon's own server code never authors an `integrations`
entry's content, and no integrator can write into another integrator's
entry.** Every `integrations[].attestations[]` row exists only because the
named integrator's own key signed it. A node hosting the network can relay,
store, and index that signature — but it cannot produce one on the
integrator's behalf, and it cannot let Integrator A's key author a claim
that verifies as Integrator B's.

This holds independently of any single node's honesty: every durable entry
is hash-chained and Merkle-committed into a Signed Tree Head (see
[`./settlement.md`](./settlement.md)), so any SDK or mirror can verify
authenticity for itself rather than trusting whichever node happened to
answer the request. Authenticity, validity, and recognition are also kept as
three separate questions, never collapsed into one boolean (ADR #76, see
[`./trust-model.md`](./trust-model.md)). "This attestation is genuinely
signed by Ashen Realms" is a different, independently-checkable question
from "is it still valid," which is different again from "does a given
consumer choose to recognize Ashen Realms as a trustworthy source at all."

This is also why layer 1 (identity, profile, friends, guilds) is
structurally off-limits to every integrator. Nothing in the durable event
catalogue lets a game/app/service author an `identity.created` or
`profile.updated` event, or a `friend.accepted`/`guild.*` event, under
anyone's issuer key. Only the identity's own signing key (for
identity/profile) or the relevant user-session actions (for
friends/guilds) can. There's no code path that accepts one from anywhere
else — by construction, not by a check that could be bypassed.

### Read access is not one uniform rule

Write isolation above is absolute and already true everywhere in layer 2.
Read access is not. It's easy to assume "any integrator can read any other
integrator's block space, only write is restricted," as the mirror image of
write isolation — but that's not what's actually built, and one piece of it
is a genuinely open question rather than a settled "yes":

- **A single attestation, if you already know its id, is fully public
  today with zero permission check.** `GET /attestations/{id}`
  (`crates/server/src/attestations.rs::get_attestation`) takes no auth
  header at all — verifying a specific claim is a public fact, matching
  ADR #76's "authenticity is a fact, never gated behind a specific
  issuer's permission."
- **Browsing or listing *all* of a subject's attestations is a
  genuinely open, undecided question.** `GET /me/achievements` only
  exists for reading *your own* full history (bearer-authenticated as
  that identity); there is no endpoint today for "give me Nova's
  attestations" as a different caller, and the actual visibility policy
  that would govern one — an issuer-set ceiling, a subject/user
  override, or some combination — is tracked, unresolved, in issue #295
  ("per-claim attestation visibility — issuer ceiling + subject
  override"). Do not assume attestation visibility is wide open by
  default; it's explicitly still being decided.
- **An integrator's own custom, non-attestation data about a user,
  explicitly published as schema instance data, defaults to
  network-readable — decided in #381, built in #384.** An integrator
  publishing instance data against its own schema is the same shape of
  deliberate opt-in that already governs attestations, so it inherits the
  same default: public unless the publishing integrator says otherwise.
  Two overrides, checked in this order: the schema itself can declare
  `default_visibility: "private"` (closing every field by default), and
  regardless of the schema's own default, an individual field can flip
  its own visibility the other way — a private schema can still expose a
  few flavor fields, and a public schema can still hide one sensitive
  field. `docs/architecture/bindings.md`'s "Avalon stores none of
  \[a character's] attributes" statement has been amended accordingly: it
  now names this as a second explicit path alongside attestations, not
  the only one. This does **not** change anything about an integrator's own
  *unpublished, internal* profile of a user (its own database) — that
  stays exactly as closed as it always was; this is specifically about
  data the integrator chose to publish through Integrator Space.

So: write isolation is a hard invariant everywhere in layer 2. Read access
varies by *what* the data is — a known attestation id is public, browsing
a subject's full attestation set is undecided (#295), and an integrator's
explicitly-published schema instance data defaults to public with a
schema/field-level opt-out, enforced by `GET /identities/{id}/integrator-data`
(#381/#384) — the only remaining closed-by-
default case is an integrator's own *unpublished* internal data, which was never
reachable through Avalon at all and stays that way.

## Where the Hub fits

The Hub (`apps/hub`) is a first-party **client**, not (yet) a registered
integrator. It reads and writes layer 1 the same way any authenticated
user session does — it has no issuer key of its own, and nothing in this
document's `integrations` array represents Hub data, because the Hub has not
published anything under its own issuer identity (the way Ashen Realms
publishes `game_schema.published` — see
[`./worked-ledger-example.md`](./worked-ledger-example.md#what-an-integrators-own-custom-fact-looks-like-in-the-same-ledger)
for that pattern).

**As of this writing, there is no Hub-exclusive "block space" to publish**
— every field the Hub currently manages (profile, friends, guilds,
including the fields #372 added) is layer-1, portable, identity-owned
data, not something scoped to the Hub itself. The real candidate for genuine
Hub-local data is issue #87 (visibility/preference store, open, not
built): a per-user UI preference — which fields are hidden on this
user's own profile view, feature flags, display settings — that has no
reason to be portable to another integrator or exposed to any integrator's SDK
at all. Once #87 lands, *that* is what a Hub block-space entry would
actually contain, and this document should be updated with a real,
cited example at that point rather than a speculative one now.

## Today in the repo

- The layer-1 types cited above (`Identity`, `Profile`, `Friendship`,
  `GuildMember`) are real and implemented; see each type's own module in
  `crates/protocol/src`. `Profile`'s `banner_url`/`status`/`links`/
  `timezone`/`theme_color`/`location` fields landed via #372; `main_guild`
  landed with no ticket. `effective_main_guild` is a response-only field
  (`crates/server/src/handlers.rs::ProfileResponse`), not part of `Profile`
  itself.
- `IntegratorCategory` (issue #282/#275) is real, implemented, additive —
  defaults to `Game` for any caller that omits it.
- `IntegratorBinding` and `AchievementAttestation` are real and implemented
  (issues #83, #31/#32/#33/#84/#85).
- `GET /me/achievements` (`crates/server/src/attestations.rs`) is real but
  unpaginated and unfiltered today — a real gap for an identity with a
  large attestation history, tracked as #377, not yet fixed.
- `game_schema.published` (issue #255) is real — an integrator can publish
  its own custom data *shape*, of any kind it wants (`characters` above is
  one example, not a fixed concept), and (as of #384) that `.proto` text is
  actually parsed/validated, not stored opaque. Actual per-user instance
  data against a published schema (`game_data.published`), its visibility
  model, and the read endpoint (`GET /identities/{id}/integrator-data`) are real
  and built (#384, decided by #381).
  See [`./integrator-space.md`](./integrator-space.md)'s "Schema vs. data exposure."
- No endpoint or indexer projection assembles the full aggregate shape
  above in one response today — see the intro's caveat. Building one (a
  real "full identity view" read) is unscoped, open work, not tracked as
  any specific ticket yet.
- The Hub's own layer-2 entry does not exist because the Hub has not
  registered as an integrator with its own issuer key — see "Where the Hub
  fits" above.

## Decisions and tickets

- **Epic #67** — identity vs. integrator data boundary, the ADR this whole document illustrates.
- **#75** — durable history / promised-durable fields.
- **#76** — authenticity, validity, and recognition kept as separate questions.
- **#80 / #84** — the two-tier issuer key model.
- **#282 / #275** — `IntegratorCategory`, unifying game/app/service.
- **#83** — integrator bindings.
- **#31 / #32 / #33 / #84 / #85** — achievement issuance, authenticity, and revocation.
- **#87** — visibility/preference store; the real candidate for a future Hub block-space example.
- **#372** — the `banner_url`/`status`/`links`/`timezone`/`theme_color`/`location` profile fields. (`main_guild`/`effective_main_guild` landed with no ticket.)
- **#255** — `game_schema.published`, the real half of "A made-up integrator's full shape" above.
- **#377** — `GET /me/achievements` pagination/filtering; the real gap behind this document's "won't zillions of achievements bog this down" question.
- **#295** — per-claim attestation visibility. Open; the genuine gap behind "Read access is not one uniform rule" above.
- **#381** — decided: integrator-published custom schema instance data defaults to network-readable, with a schema-level and bidirectional field-level override. Amended `bindings.md` accordingly.
- **#384** — implementation of #381, in progress as of this writing: real protobuf parsing/validation, not opaque storage.
