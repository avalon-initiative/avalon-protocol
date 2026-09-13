# An Identity's Aggregate View: Avalon-Native Data vs. Integrator Block Space

[`./worked-ledger-example.md`](./worked-ledger-example.md) renders one
player's ledger as an ordered *event log* — the sequence of things that
happened, over time, in the actual `ProtocolEvent` envelope. This document
is different: it renders the same identity as a single **aggregate
snapshot** — what a full picture of "this identity, right now" looks like
once every relevant event has been folded together. Neither document is the
ledger itself; the ledger is the ordered event sequence
(`worked-ledger-example.md`'s subject). This one is a read-side projection
of it, the shape an indexer, an SDK, or a public API would hand back.

No single existing endpoint returns exactly the shape below today — it is
assembled conceptually from several real, separate reads (`GET /me`, the
guild-membership list, the friends list, per-subject attestations, game
bindings). Treat the shape as the intended target for a future aggregate
read, not a literal endpoint response — every field in it is a real,
cited field from an existing type; the *assembly* is illustrative.

## The two layers

**Layer 1 — Avalon-native, portable identity data.** Everything under
direct control of the identity's own owner (with two narrow exceptions:
guild role changes go through guild governance, per
[`./guilds.md`](./guilds.md); friendship requires both parties' consent,
per [`./social-graph.md`](./social-graph.md)). This is the data that
follows the identity across every game, app, and service it ever touches —
identity, profile self-description, friends, guild memberships. See
[`./identity.md`](./identity.md)'s "Self-described metadata is
self-expression, not fact."

**Layer 2 — integrator block space.** Everything scoped to one specific
game, app, or service — a *binding* (the identity's opt-in connection to
that integrator, [`./game-bindings.md`](./game-bindings.md)) plus whatever
attestations that integrator has issued about the identity under its own
issuer key ([`./achievements-and-attestations.md`](./achievements-and-attestations.md)).
Game, App, and Service are one unified concept —
`avalon_protocol::games::IntegratorCategory` (issue #282, decided #275) —
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
    "location": "Pacific Northwest"
  },
  "guilds": [
    {"guild_id": "g-1", "role": "officer", "joined_at": "2027-02-01T00:00:00Z"}
  ],
  "friends": ["a1b2c3d4-...-000002", "a1b2c3d4-...-000003"],
  "layer_2": [
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

`banner_url`/`status`/`links`/`timezone`/`theme_color`/`location` landed
via issue #372. If this doc and the actual code ever disagree, the code is
right and this doc is stale — same discipline `worked-ledger-example.md`
holds itself to.

Real field provenance, so this doc can be checked against source directly:
`identity`/`profile` from `avalon_protocol::identity::{Identity, Profile}`;
`guilds` entries from `avalon_protocol::guilds::GuildMember { guild_id,
identity_id, role, joined_at }`; `friends` derived from
`avalon_protocol::social::Friendship { a, b, since }`; `layer_2[].binding`
from `avalon_protocol::games::GameBinding { identity_id, game_id,
established_at, ended_at }`; `layer_2[].attestations` from
`avalon_protocol::achievements::AchievementAttestation { id, issuer,
subject, achievement, issued_at, proof }`; `layer_2[].type` from
`avalon_protocol::games::IntegratorCategory`.

## A made-up game's full shape, illustrated

The `layer_2` example above is deliberately minimal. Here is one entry
fleshed all the way out for a hypothetical game, **"Emberfall Online"**, to
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
      "proto_source": "message Character { uint32 level = 1; string class = 2; repeated string skills = 3; }"
    }
  ],
  "custom_data": {
    "_status": "ILLUSTRATIVE — NOT BUILDABLE TODAY, see below",
    "schema": "game:emberfall-online:schema:character:v1",
    "instance": {
      "level": 42,
      "class": "Ranger",
      "skills": ["Longshot", "Camouflage", "Rapid Fire"],
      "inventory": [
        {"item": "Ashwood Bow", "rarity": "epic"},
        {"item": "Cloak of Whispers", "rarity": "rare"}
      ]
    }
  }
}
```

Three distinct pieces, three different rules:

- **`attestations`** — real, built, and the only one of the three that is
  *canonical across every integrator*. Emberfall Online cannot invent its
  own shape for these; every entry is a `GlobalId` +
  `AchievementAttestation`, identical in structure to any other game's,
  app's, or service's attestations. This is what makes a generic "show me
  this player's achievements from anywhere" view possible at all.
- **`published_schemas`** — real, built (`game_schema.published`, issue
  #255; see
  [`./worked-ledger-example.md`](./worked-ledger-example.md#what-a-games-own-custom-fact-looks-like-in-the-same-ledger)).
  Emberfall Online *can* define its own arbitrary `Character` shape here —
  this is genuinely integrator-custom, by design, because nothing outside
  Emberfall Online is expected to know what a "Character" means for this
  specific game.
- **`custom_data`** — **illustrative only.** This is what an actual
  `Character` *instance* (Nova's real level, class, inventory) would look
  like if Game Space's data-exposure half existed. It does not exist today
  — [`./game-space.md`](./game-space.md)'s own "Schema vs. data exposure"
  section states this plainly: schema publication is built, publishing
  real per-player instance data against that schema is not. Nothing in
  this repo accepts a payload shaped like `custom_data` today. It's shown
  here so the *target* shape — schema-defined, per-integrator, sitting
  clearly apart from the canonical `attestations` array — is unambiguous
  once that work happens, not so a reader assumes it already works.

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
[`./games-and-issuers.md`](./games-and-issuers.md) for the full key
lifecycle.

`docs/architecture/security-model.md`'s "Who controls what" table states
the isolation this document's `layer_2` array depends on, plainly:

| Actor | Controls | Cannot |
|---|---|---|
| Game/App/Service | its own bindings, its own attestations under its own issuer key | touch another integrator's profile/bindings; issue under another issuer's identity; alter the identity itself, its friends, or its guild history |
| Avalon infrastructure | transport, indexing, settlement, discovery, verification | fabricate an issuer claim; fabricate an identity; silently become the owner of user or integrator data |

Concretely: **Avalon's own server code never authors a `layer_2` entry's
content, and no integrator can write into another integrator's entry.**
Every `layer_2[].attestations[]` row exists only because the named
integrator's own key signed it; a node hosting the network can relay,
store, and index that signature, but cannot produce one on the
integrator's behalf, and cannot let Integrator A's key author a claim that
verifies as Integrator B's. This is enforced independently of any single
node's honesty — every durable entry is hash-chained and Merkle-committed
into a Signed Tree Head (see [`./settlement.md`](./settlement.md)), so any
SDK or mirror can verify authenticity for itself rather than trusting
whichever node happened to answer the request. Authenticity, validity, and
recognition are then kept as three separate questions, never collapsed
into one boolean (ADR #76, see [`./trust-model.md`](./trust-model.md)) —
"this attestation is genuinely signed by Ashen Realms" is a different,
independently-checkable question from "is it still valid" or "does a given
consumer choose to recognize Ashen Realms as a trustworthy source at all."

This is also why `layer_1` (identity, profile, friends, guilds) is
structurally off-limits to every integrator: nothing in the durable event
catalogue lets a game/app/service author an `identity.created` or
`profile.updated` event, or a `friend.accepted`/`guild.*` event, under
anyone's issuer key but the identity's own signing key (for
identity/profile) or the relevant player-session actions (for
friends/guilds) — there is no code path that accepts one, by construction,
not by a check that could be bypassed.

## Where the Hub fits

The Hub (`apps/hub`) is a first-party **client**, not (yet) a registered
integrator. It reads and writes layer 1 the same way any authenticated
player session does — it has no issuer key of its own, and nothing in this
document's `layer_2` array represents Hub data, because the Hub has not
published anything under its own issuer identity (the way Ashen Realms
publishes `game_schema.published` — see
[`./worked-ledger-example.md`](./worked-ledger-example.md#what-a-games-own-custom-fact-looks-like-in-the-same-ledger)
for that pattern).

**As of this writing, there is no Hub-exclusive "block space" to publish**
— every field the Hub currently manages (profile, friends, guilds,
including the fields #372 added) is layer-1, portable, identity-owned
data, not something scoped to the Hub itself. The real candidate for genuine
Hub-local data is issue #87 (visibility/preference store, open, not
built): a per-player UI preference — which fields are hidden on this
player's own profile view, feature flags, display settings — that has no
reason to be portable to another game or exposed to any integrator's SDK
at all. Once #87 lands, *that* is what a Hub block-space entry would
actually contain, and this document should be updated with a real,
cited example at that point rather than a speculative one now.

## Today in the repo

- The layer-1 types cited above (`Identity`, `Profile`, `Friendship`,
  `GuildMember`) are real and implemented; see each type's own module in
  `crates/protocol/src`. `Profile`'s `banner_url`/`status`/`links`/
  `timezone`/`theme_color`/`location` fields landed via #372.
- `IntegratorCategory` (issue #282/#275) is real, implemented, additive —
  defaults to `Game` for any caller that omits it.
- `GameBinding` and `AchievementAttestation` are real and implemented
  (issues #83, #31/#32/#33/#84/#85).
- `GET /me/achievements` (`crates/server/src/attestations.rs`) is real but
  unpaginated and unfiltered today — a real gap for an identity with a
  large attestation history, tracked as #377, not yet fixed.
- `game_schema.published` (issue #255) is real — an integrator can publish
  its own custom data *shape*. Actual per-player instance data against
  that shape (the `custom_data` example above) is not built — see
  [`./game-space.md`](./game-space.md)'s "Schema vs. data exposure."
- No endpoint or indexer projection assembles the full aggregate shape
  above in one response today — see the intro's caveat. Building one (a
  real "full identity view" read) is unscoped, open work, not tracked as
  any specific ticket yet.
- The Hub's own layer-2 entry does not exist because the Hub has not
  registered as an integrator with its own issuer key — see "Where the Hub
  fits" above.

## Decisions and tickets

Epic #67 (identity vs. game data boundary, the ADR this whole document
illustrates), #75 (durable history / promised-durable fields), #76
(authenticity/validity/recognition kept separate), #80/#84 (two-tier
issuer key model), #282/#275 (`IntegratorCategory`, unifying game/app/
service), #83 (game bindings), #31/#32/#33/#84/#85 (achievement
issuance/authenticity/revocation), #87 (visibility/preference store — the
real candidate for a future Hub block-space example), #372 (the
`banner_url`/`status`/`links`/`timezone`/`theme_color`/`location` profile
fields), #255 (`game_schema.published`, the real half of "A made-up game's
full shape" above), #377 (`GET /me/achievements` pagination/filtering,
the real gap behind the "won't zillions of achievements bog this down"
question this document's `attestations` shape prompted).
