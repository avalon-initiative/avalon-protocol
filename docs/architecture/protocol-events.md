# Protocol Events

A protocol event is a durable fact Avalon considers part of its history. **Not
every game action is a protocol event; ordinary gameplay never becomes one.**
**Events are the canonical record — every table is a projection of them**
([#75](https://github.com/LunarVagabond/avalon-protocol/issues/75)), and
**history is append-only: a correction is a new event, never an edit.**

## Hot gameplay vs durable events

| Stays game-side (never an event) | May enter durable history |
|---|---|
| movement, combat, physics, AI | achievement issued / revoked |
| HP, XP ticks, NPC state, player position | tournament result |
| matchmaking, ordinary chat | guild created, membership / role changed |
| game-specific inventory and economy | game registered, binding established |
| typing indicators, connection state, presence | issuer registered, key rotated, suspended |
| | attestation issued / superseded, ownership transferred |

The test is the one in [`./overview.md`](./overview.md): would this fact matter
outside the game that produced it, and does Avalon promise to preserve it? If
either answer is no, it is not a protocol event. Presence in particular is never
one ([`./presence.md`](./presence.md)).

## The types

`crates/protocol/src/events.rs` defines three types, unchanged since the
scaffold:

```rust
pub struct ProtocolEvent {
    pub id: Uuid,
    pub kind: String,          // namespaced, e.g. "achievement.issued"
    pub issuer: GlobalId,      // who asserts this fact
    pub subject: GlobalId,     // what/whom it is about
    pub payload: serde_json::Value,
    pub timestamp: OffsetDateTime,
    pub version: u32,          // payload schema version for this kind
}

pub struct EventBatch { pub id: Uuid, pub events: Vec<ProtocolEvent>, pub created_at: OffsetDateTime }
pub struct Commitment { pub batch_id: Uuid, pub proof: Vec<u8>, pub committed_at: OffsetDateTime }
```

`kind` is a string rather than a closed enum so a new kind does not require a
protocol version bump. `Commitment.proof` is deliberately opaque: `protocol` does
not know whether it is a ledger hash, a signed tree head, or a Merkle root
anchored elsewhere.

## The pipeline

```text
Protocol Event
      |
      +----> Query Projection          (indexer; rebuildable)
      |
      +----> Event Buffer
                  |
                  v
              Batching                  (#38)
                  |
                  v
          Commitment / Merkle Root      (#40)
                  |
                  v
              Settlement                (SettlementProvider; #79 for the backend)
```

An event fans out to the read model and to the settlement path. The projection
is the optimized copy; the settled history is the record. Never one event = one
settlement transaction — see [`./settlement.md`](./settlement.md).

## Design properties

Every durable event must be able to be:

- **signed** — by the actor asserting it (an issuer's key for attestations, the
  player's key for identity and profile claims once
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) lands)
- **verified** — signature against the key that was valid at `timestamp`
- **indexed** — applied idempotently to a projection
- **replayed** — in order, from genesis, to rebuild any projection
- **committed** — batched behind a durable commitment
- **revoked or corrected** — by a later event referencing it, never by mutation
- **versioned** — decodable for as long as the log exists
- **audited** — issuer, subject, timestamp, and position in the log are all
  independently checkable

Events carry enough canonical information to reconstruct required state and no
more. Anything derivable from other events is computed by the indexer, not
stored twice.

## Kind catalogue (proposed)

Naming: `<domain>.<past-tense-verb>`. This table is the starting point that
[#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) finalizes;
until then it is proposed, not normative. "Network" as signer means the node
records the fact on behalf of an authenticated actor and is the
milestone-1 stand-in until actor signatures exist.

| Kind | Issuer → subject | Payload (canonical) | Drives | Signed by |
|---|---|---|---|---|
| `identity.created` | identity → identity | identity id | identities | identity key (#73); network today |
| `profile.updated` | identity → identity | changed promised-durable fields | profiles | identity key |
| `game.registered` | game → game | slug, name, developer, requested capabilities, initial key | games, registry | game key |
| `game.binding_established` | identity → game | identity, game | bindings, registry players | identity key |
| `game.binding_ended` | identity → game | binding ref | bindings | identity key |
| `issuer.registered` | issuer → issuer | issuer id, initial key set | issuers | issuer key |
| `issuer.key_added` | issuer → issuer | key id, public key, algorithm, validity window | issuer keys | existing issuer key |
| `issuer.key_revoked` | issuer → issuer | key id, reason (`rotated`, `compromised`, …) | issuer keys | issuer key |
| `issuer.key_expired` | issuer → issuer | key id | issuer keys | issuer key or network |
| `issuer.suspended` / `.reinstated` / `.revoked` / `.deprecated` | network or issuer → issuer | reason, effective at | issuer status | operator (audited) or issuer |
| `friend.requested` / `.accepted` / `.removed` | identity → identity | the two identities, actor | friendships | acting identity's key — *only if friendships are classified promised-durable (#86)* |
| `guild.created` | identity → guild | name, tag, description, founder | guilds | founder key |
| `guild.member_added` / `.member_removed` | guild → identity | role, actor | rosters, history | acting member's key |
| `guild.role_changed` | guild → identity | old role, new role, actor | rosters, history | acting member's key |
| `guild.game_associated` | guild → game | guild, game | associations | guild officer key |
| `achievement.defined` | game → achievement id | name, description, schema | definitions | game key |
| `achievement.issued` | game → identity | achievement id, attestation id, evidence ref | attestations | issuer key |
| `achievement.revoked` | game → attestation | attestation ref, reason code, reason | attestation status | issuer key |
| `attestation.superseded` | game → attestation | old ref, new ref | attestation status | issuer key |
| `tournament.result_issued` | game → identity | `achievement.issued` with the tournament schema | attestations, registry | issuer key |
| `recognition.published` | game → issuer | recognized claim types / scopes | recognition graph | game key |

Conventions: `issuer` and `subject` are `GlobalId`s
(`crates/protocol/src/ids.rs`) — namespaced, e.g.
`game:ashen-realms:achievement:dragon_slayer`, so two games' `dragon_slayer`
never collide ([`./provenance.md`](./provenance.md)). Each kind has exactly one
payload schema per version.

## Versioning policy

History may outlive every current maintainer. Therefore:

- Adding an optional field does not change `version`.
- Removing, renaming, or changing the meaning of a field bumps `version`.
- Every version ever emitted stays decodable forever. Decoders are added, never
  deleted.
- An unknown kind is preserved and skipped by an indexer that does not
  understand it; it is never dropped from the log.
- Attestations additionally carry an issuer-declared schema reference so a
  consumer can recognize "tournament result, schema v1" independently of the
  issuer's naming.

"We can change the schema later" is true of a projection table and false of the
log.

## History vs current state

Both are kept, and kept distinct:

```text
Guild membership history (events)        Current projection (indexer)
2027-01-01  X joins Guild A              Player X
2027-04-14  X becomes Officer                Guild: none
2028-02-10  X leaves Guild A
```

History is reconstructable; current state is optimized for reads and can always
be thrown away and rebuilt. A "current status" column on a projection row (e.g.
an attestation's revoked flag) is a cache of the latest relevant event, never
the record — [`./revocation.md`](./revocation.md).

## Today in the repo

- Types: `crates/protocol/src/events.rs` as shown above.
- Exactly one emitter: `register` in `crates/server/src/handlers.rs` writes
  `identity.created` with `issuer = network:avalon-server:system:registration`
  and a payload containing `identity_id` and `username`. The `username` field
  disappears with #73; the node-as-issuer shape is a milestone-1 stand-in noted
  on that issue.
- `profile.updated` does not exist yet — `update_profile` writes Postgres only
  ([#86](https://github.com/LunarVagabond/avalon-protocol/issues/86)).
- The ledger row shape is `crates/server/db/migrations/0002_ledger/up.sql`; the
  content hash covers `event_id`, `kind`, `issuer`, `subject`, `payload`,
  `timestamp`, `version` (`crates/chain/src/postgres.rs`).
- No typed kinds, no payload structs, no catalogue in code.

## Decisions and tickets

- #75 durable history is canonical
- #82 event kind catalogue and versioning policy
- [#38](https://github.com/LunarVagabond/avalon-protocol/issues/38) batching
- [#71](https://github.com/LunarVagabond/avalon-protocol/issues/71) events must
  commit atomically with the projection change
- #86 profile events; #73 identity signs its own events
- [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) revocation
  entry shape
