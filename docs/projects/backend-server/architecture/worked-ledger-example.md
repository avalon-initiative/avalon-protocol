# A Worked Example: One User's Ledger

[`./protocol-events.md`](./protocol-events.md) catalogues every event kind
with its payload shape in the abstract; this document renders one
hypothetical user's ledger as a real, ordered sequence of `ProtocolEvent`
JSON instances, in the actual envelope every event uses
(`id`/`kind`/`issuer`/`subject`/`payload`/`timestamp`/`version` —
`crates/protocol/src/events.rs`). It exists for anyone who needs to see
what "the ledger" concretely looks like without reconstructing it
themselves from the event-kind table plus separate code reading — a new
contributor, a prospective integrator, a hoster.

Every payload below is copied from the actual `serde_json::json!` call site
that builds it, not reverse-engineered from the schema
table — if a payload here and its source ever disagree, the source is
right and this doc is stale.

## The cast

- **Nova** — a user, identity id `a1b2c3d4-...-000001`.
- **Ashen Realms** (`ashen-realms`) — a `Game`-category issuer Nova plays.
- **The Wandering Blades** — a guild Nova founds.

## The sequence

### 1. Nova creates an identity

`register_finish` (`crates/server/src/handlers.rs`) — the only event kind
genuinely signed by the identity's own key rather than network-attributed
at this point in the sequence, since it's the event that brings the
identity's key into existence in the first place.

```json
{
  "id": "e0000000-0000-0000-0000-000000000001",
  "kind": "identity.created",
  "issuer": "identity:a1b2c3d4-...-000001:self:created",
  "subject": "identity:a1b2c3d4-...-000001:self:created",
  "payload": {
    "identity_id": "a1b2c3d4-...-000001",
    "display_name": "Nova"
  },
  "timestamp": "2027-01-04T09:12:03Z",
  "version": 1
}
```

No `username` field exists anywhere in this or any later event —
`display_name` (`Nova`, above) is the whole handle itself, globally
unique and case-insensitive, no discriminator suffix. See [`./identity.md`](./identity.md).

### 2. Nova sets a bio and pronouns

`update_profile` (`crates/server/src/handlers.rs`) — only the fields that
actually changed ride in the payload; an untouched field is simply absent
from the object, not `null` (`null` means an explicit clear — see
`profile_updated_payload`'s own doc comment).

```json
{
  "id": "e0000000-0000-0000-0000-000000000002",
  "kind": "profile.updated",
  "issuer": "identity:a1b2c3d4-...-000001:self:profile_updated",
  "subject": "identity:a1b2c3d4-...-000001:self:profile_updated",
  "payload": {
    "bio": "Full-time dragon slayer, part-time guild officer.",
    "pronouns": "she/her"
  },
  "timestamp": "2027-01-04T09:18:47Z",
  "version": 1
}
```

### 3. Nova connects to Ashen Realms

Ashen Realms itself already exists on the network from its own earlier
`game.registered` event (written once, at the integrator's own registration —
not repeated here since this is Nova's ledger walkthrough, not Ashen
Realms'; see [`./protocol-events.md`](./protocol-events.md#current-implementation)
for that payload's shape). Nova connecting to it
(`crates/server/src/connections.rs`) is its own event:

```json
{
  "id": "e0000000-0000-0000-0000-000000000003",
  "kind": "game.binding_established",
  "issuer": "identity:a1b2c3d4-...-000001:self:binding_established",
  "subject": "game:ashen-realms:self:binding_established",
  "payload": {
    "binding_id": "b1000000-0000-0000-0000-000000000001",
    "identity_id": "a1b2c3d4-...-000001",
    "integrator_id": "9f000000-0000-0000-0000-00000000ash1",
    "slug": "ashen-realms"
  },
  "timestamp": "2027-01-10T14:02:11Z",
  "version": 1
}
```

Connecting typically grants one or more capabilities in the same request —
each is its own `permission.granted` event (one per capability, not a list
inside the binding event), omitted here for brevity; see
[`./protocol-events.md`](./protocol-events.md#current-implementation)'s
`connections.rs` entry for that payload's exact shape.

### 4. Ashen Realms slays a dragon: an achievement is issued

This is two prior events (not repeated here, since they belong to Ashen
Realms' own catalogue, not Nova's binding to it) plus one that names Nova:
Ashen Realms first defines the `dragon_slayer` achievement
(`achievement.defined`, `crates/server/src/achievements.rs`), then issues
it to Nova once she earns it. Issuance is the first event kind in this
whole walkthrough that carries a **real embedded signature** — Ashen
Realms' own Ed25519 key signs the attestation itself, not just the HTTP
request that submitted it (see
[`./achievements-and-attestations.md`](./achievements-and-attestations.md#issuance-is-signed-not-merely-authenticated)):

```json
{
  "id": "e0000000-0000-0000-0000-000000000004",
  "kind": "achievement.issued",
  "issuer": "game:ashen-realms:self:achievement_issued",
  "subject": "identity:a1b2c3d4-...-000001:self:achievement_issued",
  "payload": {
    "id": "at100000-0000-0000-0000-000000000001",
    "issuer": "game:ashen-realms",
    "subject": "a1b2c3d4-...-000001",
    "achievement": "game:ashen-realms:achievement:dragon_slayer",
    "evidence": { "replay_id": "r-88213" },
    "proof": {
      "key_id": "k2000000-0000-0000-0000-000000000001",
      "algorithm": "ed25519",
      "bytes": "MEUCIQDx3f...base64-encoded-detached-signature...="
    }
  },
  "timestamp": "2027-02-02T20:47:31Z",
  "version": 1
}
```

`evidence` is an unverified, optional pointer the issuer supplies (a replay
id here) — carried through for context, never itself part of what's signed
or a basis for authenticity ([`./achievements-and-attestations.md`](./achievements-and-attestations.md)).
`achievement` is the definition's namespaced `GlobalId`
(`game:<slug>:achievement:<key>`), not a bare name — this is exactly the
string Nova's `GET /me/achievements` response carries, resolved to a
display name client-side by a separate lookup
(`apps/hub/src/api/achievements.ts`), never embedded here.

### 5. Nova founds a guild and adds a member

`guild.created` (`crates/server/src/guilds.rs`):

```json
{
  "id": "e0000000-0000-0000-0000-000000000005",
  "kind": "guild.created",
  "issuer": "identity:a1b2c3d4-...-000001:self:guild_created",
  "subject": "guild:g3000000-0000-0000-0000-000000000001:self:guild_created",
  "payload": {
    "guild_id": "g3000000-0000-0000-0000-000000000001",
    "name": "The Wandering Blades",
    "tag": "WB",
    "description": "Casual raiders, EU evenings.",
    "owner": "a1b2c3d4-...-000001"
  },
  "timestamp": "2027-02-15T18:30:00Z",
  "version": 1
}
```

A friend, Kestrel, accepts an invite and joins — `guild.member_added`:

```json
{
  "id": "e0000000-0000-0000-0000-000000000006",
  "kind": "guild.member_added",
  "issuer": "identity:b7000000-0000-0000-0000-00000000kes1:self:guild_member_added",
  "subject": "guild:g3000000-0000-0000-0000-000000000001:self:guild_member_added",
  "payload": {
    "guild_id": "g3000000-0000-0000-0000-000000000001",
    "identity_id": "b7000000-0000-0000-0000-00000000kes1",
    "role_index": 0,
    "via": "invite",
    "actor": "a1b2c3d4-...-000001"
  },
  "timestamp": "2027-02-16T09:05:44Z",
  "version": 1
}
```

Note `issuer` here is Kestrel's own identity (the member who joined), while
`actor` inside the payload is Nova (who sent the invite Kestrel accepted) —
`issuer` says whose event this is on the ledger, the payload's own fields
say who caused what within it; the two aren't always the same identity.

## What an integrator's own custom fact looks like in the same ledger

Ashen Realms can also publish its own data model — a versioned `.proto`
schema description — into this same ledger, same envelope, a completely
different `kind` (`game_schema.published`,
`crates/server/src/integrator_schemas.rs`):

```json
{
  "id": "e0000000-0000-0000-0000-000000000007",
  "kind": "game_schema.published",
  "issuer": "game:ashen-realms:self:schema_published",
  "subject": "game:ashen-realms:schema:character:v1",
  "payload": {
    "id": "game:ashen-realms:schema:character:v1",
    "integrator_id": "9f000000-0000-0000-0000-00000000ash1",
    "slug": "ashen-realms",
    "version": 1,
    "proto_source": "message Character { uint32 level = 1; ... }",
    "supersedes": null
  },
  "timestamp": "2027-03-01T11:00:00Z",
  "version": 1
}
```

This is **shape only** — "here is how our data is structured," not by itself
an actual instance of Nova's in-game character (real instance data is
[Integrator Space](./integrator-space.md)'s own separate,
now-also-built mechanism — see that doc's "Schema vs. data exposure"
section). An integrator-defined *fact about a specific user* also lands on
the ledger through the achievement/milestone mechanism above (optionally
carrying an issuer-declared `schema` reference in its own definition,
matching a published schema's `GlobalId`). [Integrator event result
attestations](./achievements-and-attestations.md) (tournaments, seasonal
championships) are the same `achievement.issued`/`.defined` mechanism with
a game-event schema, not a separate event kind — currently on hold as a
feature, not because the mechanism can't support it.

## What never appears here

None of the following are protocol events, on this or any user's
ledger, by design — see
[`./protocol-events.md`](./protocol-events.md#hot-gameplay-vs-durable-events)
and [`./privacy.md`](./privacy.md):

- **Presence** ("Nova is online, playing Ashen Realms right now") —
  ephemeral, served from an in-memory/short-TTL store
  (`crates/server/src/presence.rs`), never written to `protocol_outbox`.
- **Guild chat, DMs, and channel messages** — operational-tier data in
  `guild_messages`/`conversations`, explicitly kept out of the ledger and
  the outbox by construction (`crate::guild_messages`'s own module doc
  comment: "this module never imports the outbox or the chain crate,"
  checked by a source grep in
  `crates/server/tests/guild_messages_no_ledger.rs`).
- **Ordinary gameplay** — HP, XP ticks, movement, combat, matchmaking,
  Ashen Realms' own in-game economy. Avalon never sees any of it unless a
  integrator deliberately chooses to describe or expose it through Integrator Space.
- **Typing indicators, connection state** — never durable, never even
  operational-tier storage beyond what a live connection needs.

A ledger reader (a mirror, an auditor, a future indexer rebuild) sees
exactly the seven events above for Nova's part of this story — nothing
about her online status right now, nothing she said in guild chat, nothing
about how she actually fights dragons mechanically. That boundary is the
whole point: Avalon durably remembers facts that matter *across* integrators and
*to* the user's own portable identity, and stays out of everything that's
just one integrator being an integrator.

## Current implementation

Every payload above is copied verbatim (field names, nesting, and the
`GlobalId` string shapes) from the real emitter it's drawn from — see
[`./protocol-events.md`](./protocol-events.md#current-implementation) for
the complete, currently-implemented emitter list this walkthrough draws
from. UUIDs and signature bytes above are illustrative placeholders, not
real; every field name and object shape they sit inside is real.
</content>
</invoke>
