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

## Field reference

**Layer 1** — `avalon_protocol::identity::{Identity, Profile}`,
`avalon_protocol::guilds::GuildMember`, `avalon_protocol::social::Friendship`:

| Field | Type | Used for |
|---|---|---|
| `identity.id` | `IdentityId` (UUID) | The stable, opaque handle everything else hangs off. Never derived from a name/username — see `identity.md`'s "The model." |
| `identity.created_at` | timestamp | When the identity came into existence (`identity.created`). |
| `profile.display_name` | string | Human-facing name; combined with `discriminator` to form the `handle` shown elsewhere. |
| `discriminator` (→ `handle`) | string | Server-chosen, disambiguates same-named identities (`name#1234`); derived into `handle`, never stored/read as its own top-level field by consumers. |
| `profile.avatar_url` | `Option<string>` | Self-chosen profile image. Validated as a well-formed `http`/`https` URL server-side (`is_http_url`). |
| `profile.banner_url` | `Option<string>` (#372) | A second image slot for a profile page header — same validation as `avatar_url`. |
| `profile.bio` | `Option<string>` | Free-text self-description, capped at 500 chars. |
| `profile.status` | `Option<string>` (#372) | A short tagline, distinct from and shorter-capped than `bio`. |
| `profile.pronouns` | `Option<string>` (#155) | Free text, capped at 40 chars. |
| `profile.favorite_genres` | `Vec<Genre>` (#155) | A fixed, small controlled vocabulary — kept useful for matching/filtering, not free text. |
| `profile.links` | `Vec<string>` (#372) | Up to 5 self-reported URLs, each validated as `http`/`https`. |
| `profile.timezone` | `Option<string>` (#372) | Self-reported only — useful for guild event scheduling. **Not** validated against the real IANA tz database yet (documented gap). |
| `profile.theme_color` | `Option<string>` (#372) | A self-chosen 6-digit hex accent color. Purely cosmetic self-expression. |
| `profile.location` | `Option<string>` (#372) | Free text only, e.g. "Pacific Northwest." **Never** IP-derived or geocoded — load-bearing constraint, not a suggestion. |
| `profile.main_guild` | `Option<GuildId>` | *Pending, in review as of this writing.* A self-chosen pointer to one of the identity's own guild memberships, so an integrator has one easy guild to build around instead of every simultaneous membership. Must reference a guild the identity is actually a member of; clears automatically if that membership ends. |
| `guilds[]` | `GuildMember { guild_id, role, joined_at }` | Every guild membership this identity currently holds, with its role in each. |
| `friends[]` | derived from `Friendship { a, b, since }` | The identity's accepted friend connections (symmetric — either side can be `a` or `b`). |

**Layer 2** — one entry per integrator binding, `avalon_protocol::games::{GameBinding, IntegratorCategory}`, `avalon_protocol::achievements::AchievementAttestation`:

| Field | Type | Used for |
|---|---|---|
| `type` | `IntegratorCategory` (game/app/service) | Which category this integrator registered as — a label, not a different mechanism; see #282/#275. |
| `integrator_id` | string/slug | Which game/app/service this entry describes. |
| `binding.established_at` | timestamp | When the identity opted into this integrator (consent-driven, identity-initiated — `game-bindings.md`). |
| `binding.ended_at` | `Option<timestamp>` | When the binding ended, if ever — history stays intact either way. |
| `attestations[].achievement` | `GlobalId` | Which claim (`game:<slug>:achievement:<key>` or `app|service:<slug>:milestone:<key>`) this attestation is about — see #324's category-driven vocabulary decision. |
| `attestations[].issued_at` | timestamp | When the issuing integrator signed this claim. |
| `attestations[].issuer_key_id` | string | Which of the issuer's operational keys signed it (#80/#84's two-tier model) — lets a compromised key be pinpointed/revoked without implicating the whole integrator. |
| `published_schemas[]` | `game_schema.published` payload (#255) | An integrator's own declared shape for its custom data (e.g. `Character`) — schema only, real and built. |
| *(instance data, e.g. `characters`)* | *decided (#381), building now (#384)* | The target shape for actual per-player instance data against a published schema — `characters` is one example; a schema can declare any shape. Public by default once published, with a schema-level `private` opt-out and a bidirectional field-level override (#381). See "A made-up game's full shape" above and `game-space.md`. |

**Not layer 1 or layer 2 at all**: presence (`status`, "last seen," current game/server) is a third, deliberately ephemeral tier — never a `ProtocolEvent`, never in this document's scope. See [`./presence.md`](./presence.md); do not add presence fields here even though they describe "this identity, right now" in a colloquial sense.

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
      "proto_source": "message Character { string name = 1; uint32 level = 2; string race = 3; string class = 4; repeated string titles = 5; }",
      "default_visibility": "private",
      "field_visibility": {"name": "public", "level": "public", "class": "public"}
    }
  ],
  "characters": {
    "_status": "BEING BUILT NOW — #384, decided #381; not yet merged as of this writing",
    "schema": "game:emberfall-online:schema:character:v1",
    "instances": [
      {
        "name": "Vesryn",
        "level": 42,
        "race": "Half-Elf",
        "class": "Ranger",
        "titles": ["Dungeon Master", "First Blood"]
      }
    ]
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
  [`./worked-ledger-example.md`](./worked-ledger-example.md#what-a-games-own-custom-fact-looks-like-in-the-same-ledger)),
  now also carrying `default_visibility`/`field_visibility` (#381/#384).
  Emberfall Online *can* define its own arbitrary `Character` shape here —
  this is genuinely integrator-custom, by design, because nothing outside
  Emberfall Online is expected to know what a "Character" means for this
  specific game — but that shape is now actually parsed and validated
  (real protobuf, not stored opaquely), not just accepted as an opaque
  string, so a malformed schema is rejected cleanly rather than silently
  stored.
- **`characters`** — **decided (#381), being built now (#384), not yet
  merged as of this writing.** This is what actual `Character`
  *instances* (Nova's real characters, per the schema Emberfall Online
  published above) look like once Game Space's data-exposure half exists.
  Deliberately narrow by design, not by limitation: the intent is small,
  portable, *fun-to-carry-across-games* flavor data — name, level, race,
  class, titles — never a character's full mechanical state (inventory,
  skills, stats used for game balance). That heavier, genuinely
  game-critical data has no reason to ever leave a game's own database;
  publishing it here would be a design mistake even once this mechanism
  exists, not just noise. Its read-access model is decided, not open: see
  "Read access is not one uniform rule" below, which now states the real
  policy rather than flagging an open question.

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

### Read access is not one uniform rule

Write isolation above is absolute and already true everywhere in layer 2.
**Read access is not** — it's easy to assume "any integrator can read any
other integrator's block space, only write is restricted" as the mirror
image of write isolation, but that's not what's actually built, and one
piece of it is a genuinely open question rather than a settled "yes":

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
  that would govern one — an issuer-set ceiling, a subject/player
  override, or some combination — is tracked, unresolved, in issue #295
  ("per-claim attestation visibility — issuer ceiling + subject
  override"). Do not assume attestation visibility is wide open by
  default; it's explicitly still being decided.
- **An integrator's own custom, non-attestation data about a player,
  explicitly published as schema instance data, defaults to
  network-readable — decided in #381, building now as #384.** A game
  publishing instance data against its own schema is the same shape of
  deliberate opt-in that already governs attestations, so it inherits the
  same default: public unless the publishing integrator says otherwise.
  Two overrides, checked in this order: the schema itself can declare
  `default_visibility: "private"` (closing every field by default), and
  regardless of the schema's own default, an individual field can flip
  its own visibility the other way — a private schema can still expose a
  few flavor fields, and a public schema can still hide one sensitive
  field. `docs/architecture/game-bindings.md`'s "Avalon stores none of
  \[a character's] attributes" statement has been amended accordingly: it
  now names this as a second explicit path alongside attestations, not
  the only one. This does **not** change anything about a game's own
  *unpublished, internal* profile of a player (its own database) — that
  stays exactly as closed as it always was; this is specifically about
  data the integrator chose to publish through Game Space.

So: write isolation is a hard invariant everywhere in layer 2. Read access
varies by *what* the data is — a known attestation id is public, browsing
a subject's full attestation set is undecided (#295), and an integrator's
explicitly-published schema instance data defaults to public with a
schema/field-level opt-out (#381/#384) — the only remaining closed-by-
default case is a game's own *unpublished* internal data, which was never
reachable through Avalon at all and stays that way.

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
  its own custom data *shape*, of any kind it wants (`characters` above is
  one example, not a fixed concept). Actual per-player instance data
  against a published schema, and its visibility model, is decided
  (#381) and being built now (#384) — not yet merged as of this writing.
  See [`./game-space.md`](./game-space.md)'s "Schema vs. data exposure."
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
question this document's `attestations` shape prompted), #295 (per-claim
attestation visibility — open, the genuine gap behind "Read access is not
one uniform rule" above), #381 (decided — integrator-published custom
schema instance data defaults to network-readable, with a schema-level
and bidirectional field-level override; amended `game-bindings.md`
accordingly), #384 (implementation, in progress as of this writing —
real protobuf parsing/validation, not opaque storage, per that ticket's
own amendment).
